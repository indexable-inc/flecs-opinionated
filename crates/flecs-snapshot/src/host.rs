//! Host worlds (SPEC.md, "Host worlds"): a Flecs world an application owns, saved as host records keyed by the
//! application's own id component, restored into free state, and migrated in place.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::CString;

use cid::Cid;
use flecs_c as sys;
use ipld_core::ipld::Ipld;
use sys::{ecs_entity_t, ecs_id_t, ecs_world_t};

use crate::cbor;
use crate::check;
use crate::dag::{self, BlockStore, Blocks, Kind, VERSION, cid_of};
use crate::described::Resolve;
use crate::migrate::{self, Hash, Migrations, Report};
use crate::mst;
use crate::schema::{self, Schema};
use crate::walk::{HostCfg, Walk, encode};
use crate::world::{grow, make_value, write_at};

pub type R<T> = Result<T, String>;

/// Which saved entities a call covers.
pub enum Selection {
    /// Every entity with the select tag (prefabs never).
    Tagged,
    /// The entities with the select tag in one subtree, its root included.
    Subtree(ecs_entity_t),
}

/// How a ref to an entity outside the selection is written.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Outside {
    /// Refused, when the entity has an id (it should be saved too).
    #[default]
    Refuse,
    /// Written as `{"id"}`.
    ById,
}

#[derive(Clone, Default)]
pub struct Encode {
    pub outside: Outside,
    /// Which named entities outside the selection may be written as `{"path"}` (none: any named one).
    pub naming: Option<Naming>,
}

/// The entities a document may name by path: only those the host makes at startup whatever a document holds
/// (prefabs, components, catalog entities). A path to an entity made at run time would bind nothing, or another
/// entity, in the next process, so it is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Naming {
    /// Top-level scopes whose entities, prefabs aside, are made at run time (`people`, `ships`).
    pub runtime_roots: Vec<String>,
    /// Scopes, at any depth, whose descendants are made at run time.
    pub runtime_scopes: Vec<String>,
}

/// One saved entity's record ([`Host::encode_parts`]): its id, its DAG-CBOR record and the layout records (by type
/// path) the record uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub id: [u8; 16],
    pub bytes: Vec<u8>,
    pub layouts: BTreeMap<String, Ipld>,
}

/// One saved entity's outcome in [`Host::encode_parts`]: its id when it has one, and its record or why it has none.
pub struct Encoded {
    pub id: Option<[u8; 16]>,
    pub part: R<Part>,
}

/// How a restore treats saved ids.
#[derive(Default)]
pub enum Ids {
    /// Keep them; an id a live entity holds is refused.
    #[default]
    Keep,
    /// Give each saved id a fresh one; refs to saved entities follow.
    Remint(Box<dyn FnMut([u8; 16]) -> [u8; 16]>),
}

#[derive(Default)]
pub struct Restore {
    pub ids: Ids,
    /// Read every entity back and compare it with its record.
    pub verify: bool,
}

#[derive(Debug)]
pub struct Restored {
    /// (id, entity) of every entity made, in the document's order; the id is the new one when reminted.
    pub entities: Vec<([u8; 16], ecs_entity_t)>,
    pub report: Report,
}

/// One saved entity's cached block (SPEC.md, "Incremental hashing").
struct Cached {
    table: usize,
    id: [u8; 16],
    cid: Cid,
    bytes: Vec<u8>,
    used: Vec<ecs_entity_t>,
    /// The entity's component bytes when every component is plain data (`Walk::is_pod`): an entity in a changed
    /// table whose bytes are equal is reused.
    raw: Option<Vec<u8>>,
    /// Made by this call (not reused), so its block is new.
    new: bool,
    /// Its saved children in Flecs's order, when its record lists them: a reorder or a `Parent` child moves no table.
    kids: Option<Vec<ecs_entity_t>>,
    /// It had DontFragment parts, which live outside its table, so its bytes are compared on every call.
    sparse: bool,
}

#[derive(Default)]
struct Cache {
    /// Table pointer to (its type, its change counters) when last seen.
    tables: HashMap<usize, (Vec<ecs_id_t>, Vec<i32>)>,
    entities: HashMap<ecs_entity_t, Cached>,
    /// Type to its layout hash when last computed.
    hashes: HashMap<ecs_entity_t, Ipld>,
    outside: Option<Outside>,
    mst: mst::Memo,
    pod: HashMap<ecs_entity_t, bool>,
}

pub struct Host {
    w: *mut ecs_world_t,
    s: Schema,
    ids_path: String,
    select: ecs_entity_t,
    cache: Cache,
}

/// A path's segments, unescaped (SPEC.md, "Values": `\` escapes `.`, `\` and a leading `#`).
pub(crate) fn segments(path: &str) -> R<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = path.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => cur.push(
                it.next()
                    .ok_or_else(|| format!("path {path:?} ends in a lone \\"))?,
            ),
            '.' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    if out.iter().any(String::is_empty) {
        return Err(format!("path {path:?} has an empty segment"));
    }
    Ok(out)
}

/// The entity at a path in a host world, segment by segment by name (a host world's refs never use `#n`).
pub(crate) fn lookup(w: *const ecs_world_t, path: &str) -> R<ecs_entity_t> {
    let mut e = 0;
    for seg in segments(path)? {
        if seg.starts_with('#') && seg[1..].bytes().all(|b| b.is_ascii_digit()) && seg.len() > 1 {
            // An escaped `\#n` unescapes to `#n`, a real name; an unescaped one never reaches here as a name.
        }
        let c =
            CString::new(seg.as_str()).map_err(|_| format!("path {path:?} contains a NUL byte"))?;
        // SAFETY: plain lookup with a NUL-terminated name.
        e = unsafe { sys::ecs_lookup_child(w, e, c.as_ptr()) };
        if e == 0 {
            return Err(format!("no entity named {path}"));
        }
    }
    Ok(e)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn type_ids(w: *const ecs_world_t, e: ecs_entity_t) -> Vec<ecs_id_t> {
    // SAFETY: `e` is alive; the type array has `count` ids.
    unsafe {
        let t = sys::ecs_get_type(w, e);
        if t.is_null() || (*t).count == 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts((*t).array, (*t).count as usize).to_vec()
    }
}

/// The bytes of every component `e` has (owned or inherited), in type order, then each DontFragment id it owns
/// (`sparse`, outside its table) with its bytes; None when one is not plain data.
fn raw_of(
    w: *mut ecs_world_t,
    walk: &mut Walk<'_>,
    pod: &mut HashMap<ecs_entity_t, bool>,
    e: ecs_entity_t,
    sparse: &[ecs_id_t],
) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let ids = type_ids(w, e);
    let in_table = ids.len();
    for (k, id) in ids.into_iter().chain(sparse.iter().copied()).enumerate() {
        if k >= in_table {
            // A DontFragment part can come or go without moving the table, so its id is part of the bytes.
            out.extend_from_slice(&id.to_le_bytes());
        }
        // Ids with flags other than a pair (a value pair such as `(ParentDepth, n)`, a toggle) hold no value.
        if id & sys::ECS_ID_FLAGS_MASK != 0 && !sys::is_pair(id) {
            continue;
        }
        // SAFETY: plain query.
        let ty = unsafe { sys::ecs_get_typeid(w, id) };
        if ty == 0 {
            continue;
        }
        // `Parent` (the non-fragmenting parent, an entity id) is plain data whatever Flecs meta says of it: every
        // `Parent` child at one depth shares a table, so reading it as not plain would re-encode them all.
        // SAFETY: a C global set by ecs_init.
        let is = ty == unsafe { sys::FLECS_IDEcsParentID_ }
            || *pod.entry(ty).or_insert_with(|| walk.is_pod(ty));
        if !is {
            return None;
        }
        let size = walk.component_size(ty).ok()?;
        // SAFETY: plain query; the value is `size` bytes, valid while the world is unchanged.
        let p = unsafe { sys::ecs_get_id(w, e, id) }.cast::<u8>();
        if p.is_null() {
            return None;
        }
        // SAFETY: as above.
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(p, size) });
    }
    Some(out)
}

fn dirty(w: *mut ecs_world_t, table: *mut sys::ecs_table_t) -> Vec<i32> {
    let mut buf = vec![0i32; 64];
    loop {
        // SAFETY: `buf` has room for its length.
        let n = unsafe { sys::glue_table_dirty(w, table, buf.as_mut_ptr(), buf.len() as i32) };
        if n as usize <= buf.len() {
            buf.truncate(n.max(0) as usize);
            return buf;
        }
        buf = vec![0; n as usize];
    }
}

/// Refs in a restored world: saved ids (after reminting) to the entities made for them, other ids to live entities,
/// paths by name.
struct HostIds<'a> {
    w: *mut ecs_world_t,
    made: &'a HashMap<[u8; 16], ecs_entity_t>,
    remint: &'a HashMap<[u8; 16], [u8; 16]>,
    live: &'a HashMap<[u8; 16], ecs_entity_t>,
}

impl Resolve for HostIds<'_> {
    fn entity(&mut self, r: &Ipld) -> R<ecs_entity_t> {
        let Ipld::Map(m) = r else {
            return Err("not a ref".into());
        };
        if let Some(Ipld::Bytes(id)) = m.get("id") {
            let id: [u8; 16] = id.as_slice().try_into().map_err(|_| "id is not 16 bytes")?;
            let id = self.remint.get(&id).copied().unwrap_or(id);
            return self
                .made
                .get(&id)
                .or_else(|| self.live.get(&id))
                .copied()
                .ok_or_else(|| format!("no entity has the id {}", hex(&id)));
        }
        if let Some(Ipld::String(p)) = m.get("path") {
            return lookup(self.w, p);
        }
        Err("not a ref".into())
    }
}

/// Where a saved layout's values go: the live type, and the chain of hashes to its record (one hash: no change).
struct Target {
    ty: ecs_entity_t,
    path: String,
    chain: Vec<Hash>,
}

/// A saved value: its bytes in the document (saved with the live layout), or migrated.
enum Val<'a> {
    Raw(&'a [u8]),
    Ipld(Ipld),
}

impl Val<'_> {
    fn to_ipld(&self) -> R<Ipld> {
        match self {
            Val::Raw(b) => cbor::decode(b),
            Val::Ipld(v) => Ok(v.clone()),
        }
    }
}

/// (rel, tgt, data: (type, value)).
type PlanPair<'a> = (Ipld, Ipld, Option<(ecs_entity_t, Val<'a>)>);

/// The saved entities' records by id, and the layouts they use.
type Records = (Vec<([u8; 16], Ipld)>, BTreeMap<String, Ipld>);

/// A record's parts, checked and converted, ready to write.
struct Plan<'a> {
    id: [u8; 16],
    /// The record's bytes in the document.
    raw: &'a [u8],
    name: Option<String>,
    parent: Option<Ipld>,
    /// The parent is held in Flecs's `Parent` component.
    parent_component: bool,
    /// Its saved children in Flecs's order (SPEC.md, "Host record").
    children: Option<Vec<Ipld>>,
    isa: Vec<Ipld>,
    tags: Vec<Ipld>,
    /// (rel, tgt, data: (type, value))
    pairs: Vec<PlanPair<'a>>,
    /// (type, value)
    components: Vec<(ecs_entity_t, Val<'a>)>,
}

/// A host document read without its values (SPEC.md, "Host document"): the id component, the layout records, and
/// each record with its values left as bytes.
struct RawDoc<'a> {
    ids: String,
    layouts: Ipld,
    recs: Vec<RawRec<'a>>,
}

/// A value's (type path, layout hash, bytes).
type RawValue<'a> = (&'a str, Ipld, &'a [u8]);

struct RawRec<'a> {
    id: [u8; 16],
    raw: &'a [u8],
    isa: Vec<Ipld>,
    name: Option<String>,
    tags: Vec<Ipld>,
    pairs: Vec<(Ipld, Ipld, Option<RawValue<'a>>)>,
    parent: Option<Ipld>,
    children: Option<Vec<Ipld>>,
    components: Vec<RawValue<'a>>,
    parent_component: bool,
}

/// Resolves nothing: values are only checked, not written.
struct NoIds;

impl Resolve for NoIds {
    fn entity(&mut self, _: &Ipld) -> R<ecs_entity_t> {
        Err("no refs are resolved while checking".into())
    }
}

/// Reads a host document's structure in DAG-CBOR order (ids, layouts, version, entities; a record's id, isa, name,
/// tags, pairs, parent, children, components, parent_component, the last two keys optional), checking everything but
/// the values, which `Walk::read_into` checks.
fn parse_doc(b: &[u8]) -> R<RawDoc<'_>> {
    let mut d = cbor::Decoder::new(b);
    let n = d.expect(5, "the host document (a map)")?;
    if n != 4 {
        return Err(format!(
            "the host document has {n} keys, not ids, layouts, version and entities"
        ));
    }
    d.key("ids")?;
    let ids = d.str()?.to_owned();
    d.key("layouts")?;
    let layouts = d.value(1)?;
    d.key("version")?;
    let v = d.int()?;
    if v != VERSION as i128 {
        return Err(format!(
            "format version {v} is not supported (this reader knows {VERSION})"
        ));
    }
    d.key("entities")?;
    let n = d.expect(4, "entities (a list)")?;
    let mut recs: Vec<RawRec<'_>> = Vec::new();
    for i in 0..n {
        let r = raw_record(&mut d).map_err(|e| format!("entity {i}: {e}"))?;
        if recs.last().is_some_and(|l| l.id >= r.id) {
            return Err(format!(
                "entity {i}: entities are not in increasing id order"
            ));
        }
        recs.push(r);
    }
    if !d.done() {
        return Err("bytes after the document".into());
    }
    Ok(RawDoc { ids, layouts, recs })
}

fn raw_ref(d: &mut cbor::Decoder<'_>, what: &str) -> R<Ipld> {
    let v = d.value(1)?;
    check::reference(&v, what)?;
    Ok(v)
}

fn raw_refs(d: &mut cbor::Decoder<'_>, what: &str) -> R<Vec<Ipld>> {
    let n = d.expect(4, &format!("{what}s (a list)"))?;
    (0..n).map(|_| raw_ref(d, what)).collect()
}

fn raw_record<'a>(d: &mut cbor::Decoder<'a>) -> R<RawRec<'a>> {
    let start = d.at;
    let n = d.expect(5, "a host record (a map)")?;
    if !(7..=9).contains(&n) {
        return Err(format!(
            "the host record has {n} keys, not id, isa, name, tags, pairs, parent and components (and optionally \
             children and parent_component)"
        ));
    }
    let mut keys = 7;
    d.key("id")?;
    let id: [u8; 16] = match d.value(1)? {
        Ipld::Bytes(b) => b
            .as_slice()
            .try_into()
            .map_err(|_| "host record id is not 16 bytes")?,
        _ => return Err("host record id is not 16 bytes".into()),
    };
    d.key("isa")?;
    let isa = raw_refs(d, "isa")?;
    d.key("name")?;
    let name = if d.peek()? == 0xf6 {
        d.value(1)?;
        None
    } else {
        Some(d.str()?.to_owned())
    };
    d.key("tags")?;
    let tags = raw_refs(d, "tag")?;
    d.key("pairs")?;
    let n = d.expect(4, "pairs (a list)")?;
    let mut pairs = Vec::new();
    for _ in 0..n {
        let k = d.expect(4, "a pair")?;
        let (r, t) = (raw_ref(d, "relationship")?, raw_ref(d, "target")?);
        let data = match k {
            2 => None,
            3 => {
                if d.expect(5, "pair data (a map)")? != 3 {
                    return Err("pair data is not {hash, type, value}".into());
                }
                d.key("hash")?;
                let hash = d.value(1)?;
                d.key("type")?;
                let ty = d.str()?;
                d.key("value")?;
                Some((ty, hash, d.skip()?))
            }
            _ => return Err("a pair is not [rel, tgt] or [rel, tgt, data]".into()),
        };
        pairs.push((r, t, data));
    }
    d.key("parent")?;
    let parent = if d.peek()? == 0xf6 {
        d.value(1)?;
        None
    } else {
        Some(raw_ref(d, "parent")?)
    };
    let mut children = None;
    if n > keys {
        let at = d.at;
        if d.str()? == "children" {
            let kids = raw_refs(d, "child")?;
            if kids.is_empty() {
                return Err("children is empty (it is left out when there are none)".into());
            }
            children = Some(kids);
            keys += 1;
        } else {
            d.at = at;
        }
    }
    d.key("components")?;
    let n = d.expect(5, "components (a map)")?;
    let mut components: Vec<RawValue<'a>> = Vec::new();
    for _ in 0..n {
        let ty = d.str()?;
        if components
            .last()
            .is_some_and(|l| cbor::key_order(l.0, ty) != core::cmp::Ordering::Less)
        {
            return Err("components out of DAG-CBOR key order, or repeated".into());
        }
        if d.expect(5, "a component (a map)")? != 2 {
            return Err(format!("the value of {ty} is not {{hash, value}}"));
        }
        d.key("hash")?;
        let hash = d.value(1)?;
        d.key("value")?;
        components.push((ty, hash, d.skip()?));
    }
    let mut parent_component = false;
    if n > keys {
        d.key("parent_component")?;
        if d.value(1)? != Ipld::Bool(true) {
            return Err("parent_component is not true (it is left out otherwise)".into());
        }
        if parent.is_none() {
            return Err("parent_component is set but there is no parent".into());
        }
        parent_component = true;
        keys += 1;
    }
    if keys != n {
        return Err(format!("the host record has {n} keys, but {keys} known ones"));
    }
    Ok(RawRec {
        id,
        raw: d.since(start),
        isa,
        name,
        tags,
        pairs,
        parent,
        children,
        components,
        parent_component,
    })
}

fn list(v: &Ipld) -> &[Ipld] {
    match v {
        Ipld::List(l) => l,
        _ => &[],
    }
}

/// Rewrites every `{"id": x}` ref in a read-back record whose x a remint gave back to the saved id, for comparing it
/// with its record (verify only).
fn unmint(v: &mut Ipld, back: &HashMap<[u8; 16], [u8; 16]>) {
    match v {
        Ipld::Map(m) => {
            if m.len() == 1
                && let Some(Ipld::Bytes(b)) = m.get_mut("id")
                && let Ok(id) = <[u8; 16]>::try_from(b.as_slice())
                && let Some(old) = back.get(&id)
            {
                *b = old.to_vec();
                return;
            }
            for x in m.values_mut() {
                unmint(x, back);
            }
        }
        Ipld::List(l) => l.iter_mut().for_each(|x| unmint(x, back)),
        _ => {}
    }
}

impl Host {
    /// A host over `world`, whose id component is at path `ids` (a struct of exactly `{hi: u64, lo: u64}`) and whose
    /// select tag is at path `select`. Adds the `sync` module if the world has none.
    ///
    /// # Safety
    ///
    /// `world` is a live Flecs world (the one this process links) that outlives the Host and is used by no other
    /// thread during the Host's calls.
    pub unsafe fn new(world: *mut ecs_world_t, ids: &str, select: &str) -> R<Host> {
        if world.is_null() {
            return Err("the world is null".into());
        }
        schema::ensure(world)?;
        let mut s = Schema::find(world)?;
        let id_ty = lookup(world, ids).map_err(|e| format!("the id component: {e}"))?;
        let select_e = lookup(world, select).map_err(|e| format!("the select tag: {e}"))?;
        s.id_component = id_ty;
        {
            let mut walk = Walk::new(world, &s);
            let size = walk
                .component_size(id_ty)
                .map_err(|e| format!("the id component {ids}: {e}"))?;
            let l = walk
                .layout(id_ty)
                .map_err(|e| format!("the id component {ids}: {e}"))?;
            let ok = size == 16
                && matches!(&l, Ipld::Map(m) if m.contains_key("struct") && match &m["members"] {
                    Ipld::List(ms) => ms.len() == 2
                        && ms.iter().zip(["hi", "lo"]).all(|(x, n)| matches!(x,
                            Ipld::List(x) if x.first() == Some(&Ipld::String(n.into()))
                                && x.get(1) == Some(&Ipld::String("u64".into())))),
                    _ => false,
                });
            if !ok {
                return Err(format!(
                    "the id component {ids} must be a struct of exactly {{hi: u64, lo: u64}} (16 bytes)"
                ));
            }
        }
        Ok(Host {
            w: world,
            s,
            ids_path: ids.to_string(),
            select: select_e,
            cache: Cache::default(),
        })
    }

    fn has(&self, e: ecs_entity_t, id: ecs_id_t) -> bool {
        // SAFETY: plain query.
        unsafe { sys::ecs_has_id(self.w, e, id) }
    }

    /// The saved entities of a selection, prefabs left out.
    fn saved(&self, sel: &Selection) -> R<Vec<ecs_entity_t>> {
        let w = self.w;
        // SAFETY: a C global set by ecs_init.
        let prefab = unsafe { sys::EcsPrefab };
        let all = match sel {
            Selection::Tagged => grow(|buf, cap| {
                // SAFETY: `buf` has room for `cap` ids.
                unsafe { sys::glue_with(w, self.select, buf, cap) }
            }),
            Selection::Subtree(root) => {
                // SAFETY: plain query.
                if !unsafe { sys::ecs_is_alive(w, *root) } {
                    return Err(format!(
                        "the subtree root #{} is not alive",
                        root & 0xffff_ffff
                    ));
                }
                let mut out = Vec::new();
                let mut stack = vec![*root];
                while let Some(e) = stack.pop() {
                    if self.has(e, self.select) {
                        out.push(e);
                    }
                    stack.extend(grow(|buf, cap| {
                        // SAFETY: `buf` has room for `cap` ids.
                        unsafe { sys::glue_children(w, e, buf, cap) }
                    }));
                }
                out
            }
        };
        Ok(all.into_iter().filter(|e| !self.has(*e, prefab)).collect())
    }

    fn cfg(&self, saved: &[ecs_entity_t], opts: &Encode) -> HostCfg {
        HostCfg {
            select: self.select,
            saved: saved.iter().copied().collect(),
            outside_by_id: opts.outside == Outside::ById,
            naming: opts.naming.clone(),
        }
    }

    fn err_at(&self, walk: &mut Walk<'_>, e: ecs_entity_t, m: String) -> String {
        match self.s.stable_id(self.w, e) {
            Some(id) => format!("entity {}: {m}", hex(&id)),
            None => format!(
                "entity {}: {m}",
                walk.path(e)
                    .unwrap_or_else(|_| format!("#{}", e & 0xffff_ffff))
            ),
        }
    }

    /// The saved entities' records, sorted by id, and the layouts they use.
    fn records(&self, sel: &Selection, opts: &Encode) -> R<Records> {
        let saved = self.saved(sel)?;
        let mut walk = Walk::new_host(self.w, &self.s, self.cfg(&saved, opts));
        walk.find_dont_fragment();
        let mut out = Vec::with_capacity(saved.len());
        for e in saved {
            let rec = walk
                .host_record(e)
                .map_err(|m| self.err_at(&mut walk, e, m))?;
            let Some(Ipld::Bytes(id)) = rec.get("id") else {
                unreachable!("host records have ids")
            };
            let id: [u8; 16] = id.as_slice().try_into().expect("16 bytes");
            out.push((id, Ipld::Map(rec)));
        }
        out.sort_by_key(|a| a.0);
        if let Some(w) = out.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(format!("two saved entities have the id {}", hex(&w[0].0)));
        }
        Ok((out, walk.take_layouts()))
    }

    /// The saved entities as one host document (SPEC.md, "Host document").
    pub fn document(&self, sel: &Selection, opts: &Encode) -> R<Ipld> {
        let (recs, layouts) = self.records(sel, opts)?;
        let mut doc = BTreeMap::new();
        doc.insert("version".to_string(), Ipld::Integer(VERSION as i128));
        doc.insert("ids".to_string(), Ipld::String(self.ids_path.clone()));
        doc.insert("layouts".to_string(), Ipld::Map(layouts));
        doc.insert(
            "entities".to_string(),
            Ipld::List(recs.into_iter().map(|(_, r)| r).collect()),
        );
        Ok(Ipld::Map(doc))
    }

    /// The host document as DAG-CBOR: the same bytes as `encode(&document(..))`, each record written straight from
    /// the entity's memory and spliced in.
    pub fn encode(&self, sel: &Selection, opts: &Encode) -> R<Vec<u8>> {
        let saved = self.saved(sel)?;
        let mut walk = Walk::new_host(self.w, &self.s, self.cfg(&saved, opts));
        walk.find_dont_fragment();
        let mut recs = Vec::with_capacity(saved.len());
        for e in saved {
            recs.push(
                walk.host_record_cbor(e)
                    .map_err(|m| self.err_at(&mut walk, e, m))?,
            );
        }
        recs.sort_by_key(|a| a.0);
        if let Some(w) = recs.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(format!("two saved entities have the id {}", hex(&w[0].0)));
        }
        let layouts = Ipld::Map(walk.take_layouts());
        let mut out = Vec::with_capacity(recs.iter().map(|r| r.1.len()).sum::<usize>() + 4096);
        // DAG-CBOR key order: ids, layouts, version, entities.
        cbor::map(&mut out, 4);
        cbor::text(&mut out, "ids");
        cbor::text(&mut out, &self.ids_path);
        cbor::text(&mut out, "layouts");
        cbor::ipld(&mut out, &layouts);
        cbor::text(&mut out, "version");
        cbor::int(&mut out, VERSION as i128);
        cbor::text(&mut out, "entities");
        cbor::list(&mut out, recs.len());
        for (_, b) in recs {
            out.extend_from_slice(&b);
        }
        Ok(out)
    }

    /// Every saved entity's record on its own, as `encode` would write it, with the layout records it uses: an entity
    /// that cannot be written is its own error (with its id when it has one), not the call's, so a caller can keep
    /// that entity's last good record and still save the rest. [`Host::assemble`] makes the document.
    pub fn encode_parts(&self, sel: &Selection, opts: &Encode) -> R<Vec<Encoded>> {
        let saved = self.saved(sel)?;
        let mut walk = Walk::new_host(self.w, &self.s, self.cfg(&saved, opts));
        walk.find_dont_fragment();
        let mut out = Vec::with_capacity(saved.len());
        for e in saved {
            let id = self.s.stable_id(self.w, e);
            let part = walk
                .host_record_cbor(e)
                .map_err(|m| self.err_at(&mut walk, e, m))
                .and_then(|(id, bytes)| {
                    Ok(Part {
                        id,
                        bytes,
                        layouts: walk.used_layouts()?,
                    })
                });
            out.push(Encoded { id, part });
        }
        Ok(out)
    }

    /// The host document of `parts` (from [`Host::encode_parts`], this call's or kept from an earlier one): the same
    /// bytes `encode` writes for those records. Two parts with one id, or two layouts of one type path that differ,
    /// are refused.
    pub fn assemble(&self, parts: &[&Part]) -> R<Vec<u8>> {
        let mut parts = parts.to_vec();
        parts.sort_by_key(|p| p.id);
        if let Some(w) = parts.windows(2).find(|w| w[0].id == w[1].id) {
            return Err(format!("two saved entities have the id {}", hex(&w[0].id)));
        }
        let mut layouts: BTreeMap<String, Ipld> = BTreeMap::new();
        for p in &parts {
            for (path, rec) in &p.layouts {
                match layouts.get(path) {
                    Some(have) if have != rec => {
                        return Err(format!(
                            "entity {}: its record uses another layout of {path} than the others' (a record kept \
                             from before a change of that type)",
                            hex(&p.id)
                        ));
                    }
                    Some(_) => {}
                    None => {
                        layouts.insert(path.clone(), rec.clone());
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(parts.iter().map(|p| p.bytes.len()).sum::<usize>() + 4096);
        // DAG-CBOR key order: ids, layouts, version, entities.
        cbor::map(&mut out, 4);
        cbor::text(&mut out, "ids");
        cbor::text(&mut out, &self.ids_path);
        cbor::text(&mut out, "layouts");
        cbor::ipld(&mut out, &Ipld::Map(layouts));
        cbor::text(&mut out, "version");
        cbor::int(&mut out, VERSION as i128);
        cbor::text(&mut out, "entities");
        cbor::list(&mut out, parts.len());
        for p in parts {
            out.extend_from_slice(&p.bytes);
        }
        Ok(out)
    }

    /// The saved entities as blocks (SPEC.md, "Host blocks"), recomputing only entities whose table changed since the
    /// last call (Flecs's per-table change counters; SPEC.md, "Incremental hashing").
    pub fn blocks(&mut self, sel: &Selection, opts: &Encode) -> R<Blocks> {
        self.blocks_with(sel, opts, false, false)
    }

    /// As `blocks`, but `blocks` holds only the root and the blocks this Host has not returned since its cache was
    /// last filled (the first call, `blocks_full`, or a change of options): what a store that kept every earlier
    /// result lacks.
    pub fn blocks_new(&mut self, sel: &Selection, opts: &Encode) -> R<Blocks> {
        self.blocks_with(sel, opts, false, true)
    }

    /// The saved entities as blocks, every record recomputed (and the cache refilled).
    pub fn blocks_full(&mut self, sel: &Selection, opts: &Encode) -> R<Blocks> {
        self.blocks_with(sel, opts, true, false)
    }

    fn blocks_with(
        &mut self,
        sel: &Selection,
        opts: &Encode,
        full: bool,
        only_new: bool,
    ) -> R<Blocks> {
        let w = self.w;
        let saved = self.saved(sel)?;
        let cfg = self.cfg(&saved, opts);
        let saved_set: HashSet<ecs_entity_t> = saved.iter().copied().collect();
        let Host {
            s, cache, ids_path, ..
        } = self;
        if full || cache.outside != Some(opts.outside) {
            *cache = Cache::default();
            cache.outside = Some(opts.outside);
        }
        let mut walk = Walk::new_host(w, s, cfg);
        walk.find_dont_fragment();
        // Types whose layout record changed since the last call: their users are recomputed.
        let mut changed_types = HashSet::new();
        for (ty, h) in &cache.hashes {
            // SAFETY: plain query.
            let now = if unsafe { sys::ecs_is_alive(w, *ty) } {
                walk.layout_record_of(*ty).ok().and_then(|r| match r {
                    Ipld::Map(m) => m.get("hash").cloned(),
                    _ => None,
                })
            } else {
                None
            };
            match now {
                Some(n) if n == *h => {}
                _ => {
                    changed_types.insert(*ty);
                    cache.pod.remove(ty);
                }
            }
        }
        // Tables whose type or counters changed.
        let mut tables: HashMap<usize, bool> = HashMap::new();
        let mut new_tables = HashMap::new();
        let mut entries = Vec::with_capacity(saved.len());
        let mut fresh: HashMap<ecs_entity_t, Cached> = HashMap::with_capacity(saved.len());
        for e in &saved {
            let e = *e;
            // SAFETY: plain query.
            let table = unsafe { sys::ecs_get_table(w, e) };
            let tk = table as usize;
            let changed = *tables.entry(tk).or_insert_with(|| {
                let now = (type_ids(w, e), dirty(w, table));
                let same = cache.tables.get(&tk) == Some(&now);
                new_tables.insert(tk, now);
                !same
            });
            // What changes a record without moving or dirtying its table (SPEC.md, "Incremental hashing"): its saved
            // children's order, and its DontFragment parts.
            let kids = crate::walk::ordered_children(w, e)
                .map(|all| {
                    all.into_iter()
                        .filter(|c| saved_set.contains(c))
                        .collect::<Vec<_>>()
                })
                .filter(|k| !k.is_empty());
            let sparse = walk.dont_fragment_ids(e);
            let mut raw = None;
            let reuse = cache.entities.get(&e).is_some_and(|c| {
                c.table == tk && c.kids == kids && c.used.iter().all(|t| !changed_types.contains(t))
            }) && (!(changed || !sparse.is_empty() || cache.entities[&e].sparse) || {
                raw = raw_of(w, &mut walk, &mut cache.pod, e, &sparse);
                raw.is_some() && cache.entities[&e].raw == raw
            });
            let c = if reuse {
                let mut c = cache.entities.remove(&e).expect("checked");
                c.new = false;
                if raw.is_some() {
                    c.raw = raw;
                }
                c
            } else {
                if raw.is_none() {
                    raw = raw_of(w, &mut walk, &mut cache.pod, e, &sparse);
                }
                let (id, bytes) =
                    walk.host_record_cbor(e)
                        .map_err(|m| match s.stable_id(w, e) {
                            Some(id) => format!("entity {}: {m}", hex(&id)),
                            None => format!("entity #{}: {m}", e & 0xffff_ffff),
                        })?;
                let used: Vec<_> = walk.used.iter().copied().collect();
                Cached {
                    table: tk,
                    id,
                    cid: cid_of(&bytes),
                    bytes,
                    used,
                    raw,
                    new: true,
                    kids,
                    sparse: !sparse.is_empty(),
                }
            };
            entries.push((c.id.to_vec(), c.cid));
            fresh.insert(e, c);
        }
        entries.sort();
        if let Some(p) = entries.windows(2).find(|p| p[0].0 == p[1].0) {
            return Err(format!("two saved entities have the id {}", hex(&p[0].0)));
        }
        // The layouts of every type a record uses, reused records included.
        let used: BTreeSet<ecs_entity_t> = fresh
            .values()
            .flat_map(|c| c.used.iter().copied())
            .collect();
        let mut layouts = BTreeMap::new();
        let mut new_hashes = HashMap::new();
        for ty in used {
            let rec = walk.layout_record_of(ty)?;
            let p = walk.path(ty)?;
            if let Ipld::Map(m) = &rec {
                new_hashes.insert(ty, m["hash"].clone());
            }
            layouts.insert(p, rec);
        }
        let (mroot, nodes) = mst::build_memo(&entries, &mut cache.mst, only_new)?;
        let mut root = BTreeMap::new();
        root.insert("version".to_string(), Ipld::Integer(VERSION as i128));
        root.insert("ids".to_string(), Ipld::String(ids_path.clone()));
        root.insert("layouts".to_string(), Ipld::Map(layouts));
        root.insert("entities".to_string(), Ipld::Link(mroot));
        let rb = encode(&Ipld::Map(root));
        let rc = cid_of(&rb);
        let mut out = vec![(rc, rb)];
        let mut seen = HashSet::from([rc]);
        let mut by_id: Vec<&Cached> = fresh.values().collect();
        by_id.sort_by_key(|a| a.id);
        for c in by_id {
            if (c.new || !only_new) && seen.insert(c.cid) {
                out.push((c.cid, c.bytes.clone()));
            }
        }
        for (c, b) in nodes {
            if seen.insert(c) {
                out.push((c, b));
            }
        }
        cache.tables = new_tables;
        cache.entities = fresh;
        cache.hashes = new_hashes;
        Ok(Blocks {
            root: rc,
            blocks: out,
        })
    }

    /// The layout record of the live type at `ty` (a path), as a document holds it.
    pub fn layout_record(&self, ty: &str) -> R<Ipld> {
        let t = lookup(self.w, ty)?;
        let mut walk = Walk::new(self.w, &self.s);
        walk.layout_record_of(t)
    }

    /// The live type a saved layout record converts to, and the chain to it.
    fn target(
        &self,
        walk: &mut Walk<'_>,
        m: &Migrations,
        path: &str,
        rec: &Ipld,
        live: &mut HashMap<String, Option<(ecs_entity_t, Hash)>>,
    ) -> R<Target> {
        let h = migrate::hash_of(rec)?;
        let mut live_of = |p: &str, walk: &mut Walk<'_>| -> R<Option<(ecs_entity_t, Hash)>> {
            if let Some(x) = live.get(p) {
                return Ok(*x);
            }
            let x = match lookup(self.w, p) {
                Ok(t) => Some((t, migrate::hash_of(&walk.layout_record_of(t)?)?)),
                Err(_) => None,
            };
            live.insert(p.to_string(), x);
            Ok(x)
        };
        if let Some((t, lh)) = live_of(path, walk)?
            && lh == h
        {
            return Ok(Target {
                ty: t,
                path: path.to_string(),
                chain: vec![h],
            });
        }
        // The reachable live types; the highest version wins.
        let mut best: Option<(u64, Target)> = None;
        let mut tie = false;
        for (chain, to) in m.reachable(&h) {
            let tp = migrate::path_of(to)?;
            let Some((t, lh)) = live_of(&tp, walk)? else {
                continue;
            };
            if lh != *chain.last().expect("non-empty") {
                continue;
            }
            let (_, v) = check::base_version(&tp)?;
            match &best {
                Some((bv, b)) if *bv == v && b.path != tp => tie = true,
                Some((bv, _)) if *bv >= v => {}
                _ => {
                    tie = false;
                    best = Some((
                        v,
                        Target {
                            ty: t,
                            path: tp,
                            chain,
                        },
                    ));
                }
            }
        }
        if tie {
            return Err(format!(
                "layout {path} migrates to two live types of the same version"
            ));
        }
        best.map(|(_, t)| t).ok_or_else(|| {
            let why = if live_of(path, walk).ok().flatten().is_some() {
                "the live type has another layout under the same name and there is no migration to a live type"
            } else {
                "no live type has it and there is no migration to one"
            };
            format!("layout {path} ({}): {why}", hex(&h))
        })
    }

    /// Converts a value saved with `rec` to its target, defaults from the target type's constructor.
    fn convert(
        &self,
        walk: &mut Walk<'_>,
        m: &Migrations,
        t: &Target,
        v: Ipld,
        report: &mut Report,
    ) -> R<Ipld> {
        if t.chain.len() == 1 {
            return Ok(v);
        }
        let w = self.w;
        let mut default = |rec: &Ipld| -> Option<Ipld> {
            let p = migrate::path_of(rec).ok()?;
            let ty = lookup(w, &p).ok()?;
            walk.default_value(ty).ok()
        };
        m.convert_chain(&t.chain, &v, &mut default, report)
    }

    /// Creates a host document's entities in the world, all or nothing (SPEC.md, "Restore"). Every part of the
    /// document is checked before the world changes; values whose layout is the live one are checked in place and
    /// written straight from the document's bytes into Flecs's storage, migrated ones go through `Ipld`.
    pub fn restore(&mut self, doc: &[u8], m: &Migrations, opts: &mut Restore) -> R<Restored> {
        let w = self.w;
        // SAFETY: plain query.
        if unsafe { sys::glue_is_deferred(w) } {
            return Err(
                "the world is deferred (a stage, or in a system or observer): restore outside one".into(),
            );
        }
        let raw = parse_doc(doc).map_err(|e| format!("the document: {e}"))?;
        if raw.ids != self.ids_path {
            return Err(format!(
                "the document's id component is {:?}, this world's is {}",
                raw.ids, self.ids_path
            ));
        }
        let layouts = check::layouts(Some(&raw.layouts))?;
        let mut walk = Walk::new(w, &self.s);
        let mut report = Report::default();
        let mut targets: HashMap<String, Target> = HashMap::new();
        let mut live_types = HashMap::new();
        for (p, rec) in layouts {
            let t = self.target(&mut walk, m, p, rec, &mut live_types)?;
            targets.insert(p.clone(), t);
        }
        // Check and convert every value before touching the world.
        let mut types: HashMap<ecs_entity_t, Ipld> = HashMap::new();
        let mut plans = Vec::with_capacity(raw.recs.len());
        for r in raw.recs {
            let at = |m: String| format!("entity {}: {m}", hex(&r.id));
            // The live type and, for a migrated value, its converted value.
            let mut value =
                |ty: &str, hash: &Ipld, bytes: &[u8]| -> R<(ecs_entity_t, Option<Ipld>)> {
                    let Some(Ipld::Map(rec)) = layouts.get(ty) else {
                        return Err(format!("{ty} has no layout record"));
                    };
                    if rec.get("hash") != Some(hash) {
                        return Err(format!(
                            "the value of {ty} names a layout hash it does not have"
                        ));
                    }
                    let t = &targets[ty];
                    if let std::collections::hash_map::Entry::Vacant(v) = types.entry(t.ty) {
                        v.insert(walk.layout(t.ty).map_err(|e| format!("{}: {e}", t.path))?);
                    }
                    if t.chain.len() == 1 {
                        let mut d = cbor::Decoder::new(bytes);
                        walk.read_into(t.ty, &mut d, core::ptr::null_mut(), &mut NoIds)
                            .map_err(|e| format!("{ty}: {e}"))?;
                        return Ok((t.ty, None));
                    }
                    let v = cbor::decode(bytes)?;
                    check::value(&v, &rec["layout"], check::Blobs::Links)
                        .map_err(|e| format!("{ty}: {e}"))?;
                    let v = self
                        .convert(&mut walk, m, t, v, &mut report)
                        .map_err(|e| format!("{ty}: {e}"))?;
                    crate::logical::run_checks(w, &v, &types[&t.ty])
                        .map_err(|e| format!("{ty}: {e}"))?;
                    Ok((t.ty, Some(v)))
                };
            let val = |(ty, v): (ecs_entity_t, Option<Ipld>), bytes| {
                (ty, v.map_or(Val::Raw(bytes), Val::Ipld))
            };
            let mut components = Vec::with_capacity(r.components.len());
            for (p, hash, bytes) in &r.components {
                components.push(val(
                    value(p, hash, bytes).map_err(|e| at(format!("component {e}")))?,
                    *bytes,
                ));
            }
            let mut pairs = Vec::with_capacity(r.pairs.len());
            for (rel, tgt, data) in r.pairs {
                let data = match data {
                    Some((ty, hash, bytes)) => Some(val(
                        value(ty, &hash, bytes).map_err(|e| at(format!("pair data {e}")))?,
                        bytes,
                    )),
                    None => None,
                };
                pairs.push((rel, tgt, data));
            }
            plans.push(Plan {
                id: r.id,
                raw: r.raw,
                name: r.name,
                parent: r.parent,
                parent_component: r.parent_component,
                children: r.children,
                isa: r.isa,
                tags: r.tags,
                pairs,
                components,
            });
        }
        // Ids.
        let live: HashMap<[u8; 16], ecs_entity_t> = grow(|buf, cap| {
            // SAFETY: `buf` has room for `cap` ids.
            unsafe { sys::glue_with(w, self.s.id_component, buf, cap) }
        })
        .into_iter()
        .filter_map(|e| self.s.stable_id(w, e).map(|id| (id, e)))
        .collect();
        let mut remint = HashMap::new();
        if let Ids::Remint(f) = &mut opts.ids {
            for p in &plans {
                let n = f(p.id);
                if live.contains_key(&n) {
                    return Err(format!(
                        "reminting {} gave {}, which a live entity holds",
                        hex(&p.id),
                        hex(&n)
                    ));
                }
                remint.insert(p.id, n);
            }
            let fresh: HashSet<_> = remint.values().collect();
            if fresh.len() != remint.len() {
                return Err("reminting gave two saved entities one id".into());
            }
        } else if let Some(p) = plans.iter().find(|p| live.contains_key(&p.id)) {
            return Err(format!(
                "entity {}: a live entity already holds this id (restore makes new entities; remint ids to load a \
                 second copy)",
                hex(&p.id)
            ));
        }
        let new_id = |id: &[u8; 16]| remint.get(id).copied().unwrap_or(*id);
        // Parent order and cycles, among the saved entities.
        let index: HashMap<[u8; 16], usize> =
            plans.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        let parent_of = |p: &Plan| -> Option<usize> {
            match &p.parent {
                Some(Ipld::Map(m)) => match m.get("id") {
                    Some(Ipld::Bytes(b)) => <[u8; 16]>::try_from(b.as_slice())
                        .ok()
                        .and_then(|id| index.get(&id).copied()),
                    _ => None,
                },
                _ => None,
            }
        };
        let mut depth = vec![usize::MAX; plans.len()];
        for i in 0..plans.len() {
            if depth[i] != usize::MAX {
                continue;
            }
            let mut chain = Vec::new();
            let mut on_path = HashSet::new();
            let mut cur = i;
            let base = loop {
                if depth[cur] != usize::MAX {
                    break depth[cur] + 1;
                }
                if !on_path.insert(cur) {
                    return Err(format!(
                        "entity {}: its parents form a cycle",
                        hex(&plans[i].id)
                    ));
                }
                chain.push(cur);
                match parent_of(&plans[cur]) {
                    None => break 0,
                    Some(p) => cur = p,
                }
            };
            for (k, x) in chain.iter().rev().enumerate() {
                depth[*x] = base + k;
            }
            if depth[i] >= 128 {
                return Err(format!(
                    "entity {}: deeper than Flecs's hierarchy limit (FLECS_DAG_DEPTH_MAX, 128)",
                    hex(&plans[i].id)
                ));
            }
        }
        // Child order (SPEC.md, "Host record"): a record's `children` lists exactly the records whose parent it is, and
        // a record with a child in `Parent` storage lists them.
        let mut kids_of: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, p) in plans.iter().enumerate() {
            if let Some(pi) = parent_of(p) {
                kids_of.entry(pi).or_default().push(i);
            }
        }
        for (i, p) in plans.iter().enumerate() {
            let at = |m: &str| format!("entity {}: {m}", hex(&p.id));
            let mine = kids_of.get(&i).map_or(&[][..], Vec::as_slice);
            match &p.children {
                Some(list) => {
                    let mut got = Vec::with_capacity(list.len());
                    for r in list {
                        let j = match r {
                            Ipld::Map(m) => match m.get("id") {
                                Some(Ipld::Bytes(b)) => <[u8; 16]>::try_from(b.as_slice())
                                    .ok()
                                    .and_then(|id| index.get(&id).copied()),
                                _ => None,
                            },
                            _ => None,
                        }
                        .ok_or_else(|| at("its children list an entity that is not in the document"))?;
                        got.push(j);
                    }
                    let mut want = mine.to_vec();
                    want.sort_unstable();
                    let listed = got.len();
                    got.sort_unstable();
                    got.dedup();
                    if got.len() != listed || got != want {
                        return Err(at(
                            "its children are not exactly the entities whose parent it is, each once",
                        ));
                    }
                }
                None if mine.iter().any(|&j| plans[j].parent_component) => {
                    return Err(at(
                        "a child of it is in Parent storage, but its record lists no child order",
                    ));
                }
                None => {}
            }
        }
        let mut order: Vec<usize> = (0..plans.len()).collect();
        order.sort_by_key(|i| depth[*i]);
        // Make the entities; from here on, an error deletes them.
        let mut made: HashMap<[u8; 16], ecs_entity_t> = HashMap::new();
        let mut entities = Vec::with_capacity(plans.len());
        for p in &plans {
            // SAFETY: plain constructor.
            let e = unsafe { sys::ecs_new(w) };
            let id = new_id(&p.id);
            self.s.set_stable_id(w, e, id);
            made.insert(id, e);
            entities.push((id, e));
        }
        let r = self.fill(&plans, &types, &order, &made, &remint, &live, opts.verify);
        if let Err(e) = r {
            for (_, x) in &entities {
                // SAFETY: entities this call made.
                unsafe { sys::ecs_delete(w, *x) };
            }
            return Err(e);
        }
        Ok(Restored { entities, report })
    }

    #[allow(clippy::too_many_arguments)]
    fn fill(
        &self,
        plans: &[Plan<'_>],
        types: &HashMap<ecs_entity_t, Ipld>,
        order: &[usize],
        made: &HashMap<[u8; 16], ecs_entity_t>,
        remint: &HashMap<[u8; 16], [u8; 16]>,
        live: &HashMap<[u8; 16], ecs_entity_t>,
        verify: bool,
    ) -> R<()> {
        let w = self.w;
        let mut ids = HostIds {
            w,
            made,
            remint,
            live,
        };
        let ent = |p: &Plan| made[&remint.get(&p.id).copied().unwrap_or(p.id)];
        // SAFETY: C globals set by ecs_init.
        let (child_of, is_a) = unsafe { (sys::EcsChildOf, sys::EcsIsA) };
        for &i in order {
            let p = &plans[i];
            let e = ent(p);
            let at = |m: String| format!("entity {}: {m}", hex(&p.id));
            let parent = match &p.parent {
                Some(r) => {
                    let pe = ids.entity(r).map_err(|m| at(format!("its parent: {m}")))?;
                    if p.parent_component {
                        // Flecs's non-fragmenting hierarchy: the `Parent` component holds the parent (EcsParent is one
                        // entity id), as ecs_new_w_parent makes it; Flecs appends `e` to the parent's ordered children.
                        // SAFETY: both alive (ecs_set_id refuses a dead or zero parent); the value is 8 bytes.
                        unsafe {
                            sys::ecs_set_id(
                                w,
                                e,
                                sys::FLECS_IDEcsParentID_,
                                std::mem::size_of::<ecs_entity_t>(),
                                (&raw const pe).cast(),
                            )
                        };
                    } else {
                        // SAFETY: both alive.
                        unsafe { sys::ecs_add_id(w, e, sys::pair(child_of, pe)) };
                    }
                    pe
                }
                None => 0,
            };
            if let Some(n) = &p.name {
                let c = CString::new(n.as_str())
                    .map_err(|_| at("its name contains a NUL byte".into()))?;
                // SAFETY: plain lookup and rename with a NUL-terminated name.
                // UNSURE(integrator): on master the (ChildOf, parent) name index covers `Parent` children too (the
                // reparent hook moves names between indexes), so this finds a sibling of either storage.
                unsafe {
                    if sys::ecs_lookup_child(w, parent, c.as_ptr()) != 0 {
                        return Err(at(format!("its parent already has a child named {n:?}")));
                    }
                    sys::ecs_set_name(w, e, c.as_ptr());
                }
            }
        }
        let mut walk = Walk::new_host(
            w,
            &self.s,
            HostCfg {
                select: self.select,
                saved: made.values().copied().collect(),
                outside_by_id: true,
                naming: None,
            },
        );
        walk.find_dont_fragment();
        // Each entity's parts are added as one batch (a stage, or defer on a host's 4.1.6), so Flecs moves it to its
        // final table once. Reads still go to `w`.
        // SAFETY: the world is not deferred (restore checked) and not in use elsewhere.
        let bw = unsafe { sys::glue_batch_begin(w) };
        // Per entity: the ids it lists, and its values as (id, type, value).
        let mut expected: Vec<HashSet<ecs_id_t>> = Vec::with_capacity(plans.len());
        let mut values: Vec<Vec<(ecs_id_t, ecs_entity_t, &Val<'_>)>> =
            Vec::with_capacity(plans.len());
        let r = (|| -> R<()> {
            for p in plans {
                let e = ent(p);
                let at = |m: String| format!("entity {}: {m}", hex(&p.id));
                let mut exp: HashSet<ecs_id_t> = HashSet::new();
                let mut vals = Vec::with_capacity(p.components.len());
                for b in &p.isa {
                    let be = ids.entity(b).map_err(|m| at(format!("a base: {m}")))?;
                    // SAFETY: both alive; adding IsA copies the base's Copy (once Override) parts.
                    unsafe { sys::ecs_add_id(bw, e, sys::pair(is_a, be)) };
                }
                for t in &p.tags {
                    let te = ids.entity(t).map_err(|m| at(format!("a tag: {m}")))?;
                    exp.insert(te);
                    // SAFETY: both alive.
                    unsafe { sys::ecs_add_id(bw, e, te) };
                }
                for (r, t, data) in &p.pairs {
                    let re = ids
                        .entity(r)
                        .map_err(|m| at(format!("a pair's relationship: {m}")))?;
                    let te = ids
                        .entity(t)
                        .map_err(|m| at(format!("a pair's target: {m}")))?;
                    let id = sys::pair(re, te);
                    exp.insert(id);
                    // SAFETY: both alive.
                    unsafe { sys::ecs_add_id(bw, e, id) };
                    if let Some((ty, v)) = data {
                        vals.push((id, *ty, v));
                    }
                }
                for (ty, v) in &p.components {
                    exp.insert(*ty);
                    // SAFETY: both alive.
                    unsafe { sys::ecs_add_id(bw, e, *ty) };
                    vals.push((*ty, *ty, v));
                }
                // SAFETY: both alive.
                unsafe { sys::ecs_add_id(bw, e, self.select) };
                expected.push(exp);
                values.push(vals);
            }
            Ok(())
        })();
        // SAFETY: paired with the glue_batch_begin above.
        unsafe { sys::glue_batch_end(w, bw) };
        r?;
        // The values, written in place over the constructed (or base-copied) ones.
        for (p, vals) in plans.iter().zip(&values) {
            let e = ent(p);
            for (id, ty, v) in vals {
                // SAFETY: `e` owns `id` (added above), whose storage is a live value of `ty`.
                // UNSURE(integrator): for a DontFragment part this is its sparse storage; ecs_get_mut_id goes through
                // flecs_get_component_ptr, which reads sparse component records on master.
                let ptr = unsafe { sys::ecs_get_mut_id(w, e, *id) }.cast::<u8>();
                let r = if ptr.is_null() {
                    Err("has no storage after it was added".to_string())
                } else {
                    match v {
                        Val::Raw(b) => {
                            walk.read_into(*ty, &mut cbor::Decoder::new(b), ptr, &mut ids)
                        }
                        Val::Ipld(v) => write_at(w, &self.s, *ty, ptr, v, &types[ty], &mut ids),
                    }
                };
                if let Err(m) = r {
                    return Err(format!(
                        "entity {}: {}: {m}",
                        hex(&p.id),
                        walk.path(*ty).unwrap_or_default()
                    ));
                }
                // SAFETY: as above.
                unsafe { sys::ecs_modified_id(w, e, *id) };
            }
        }
        // Parts a base copied that the record does not list (DontFragment ones included).
        for (p, exp) in plans.iter().zip(&expected) {
            let e = ent(p);
            let sparse = walk.dont_fragment_ids(e);
            for id in type_ids(w, e).into_iter().chain(sparse) {
                if id != self.select && !exp.contains(&id) && walk.listed_part(id)? {
                    // SAFETY: `id` is in `e`'s type.
                    unsafe { sys::ecs_remove_id(w, e, id) };
                }
            }
        }
        // Child order: each restored parent's restored children go to the places they hold, in the saved order.
        for p in plans {
            let Some(list) = &p.children else { continue };
            let pe = ent(p);
            let at = |m: &str| format!("entity {}: {m}", hex(&p.id));
            let want: Vec<ecs_entity_t> = list
                .iter()
                .map(|r| ids.entity(r))
                .collect::<R<_>>()
                .map_err(|m| at(&format!("a child: {m}")))?;
            let Some(cur) = crate::walk::ordered_children(w, pe) else {
                return Err(at(
                    "its record lists a child order, but Flecs keeps none for it (it needs OrderedChildren, or a \
                     child in Parent storage)",
                ));
            };
            let listed: HashSet<ecs_entity_t> = want.iter().copied().collect();
            let slots: Vec<usize> = (0..cur.len()).filter(|i| listed.contains(&cur[*i])).collect();
            if slots.len() != want.len() {
                return Err(at("its listed children are not all its children after the restore"));
            }
            let mut next = cur.clone();
            for (slot, c) in slots.iter().zip(&want) {
                next[*slot] = *c;
            }
            if next != cur {
                // SAFETY: `next` is a permutation of `pe`'s ordered children; the world is not deferred.
                unsafe { sys::ecs_set_child_order(w, pe, next.as_ptr(), next.len() as i32) };
            }
        }
        if verify {
            let back: HashMap<[u8; 16], [u8; 16]> = remint.iter().map(|(a, b)| (*b, *a)).collect();
            let mut vw = Walk::new_host(
                w,
                &self.s,
                HostCfg {
                    select: self.select,
                    saved: made.values().copied().collect(),
                    outside_by_id: true,
                    naming: None,
                },
            );
            vw.find_dont_fragment();
            for p in plans {
                let e = ent(p);
                let mut got = Ipld::Map(
                    vw.host_record(e)
                        .map_err(|m| format!("verify: entity {}: {m}", hex(&p.id)))?,
                );
                unmint(&mut got, &back);
                let Ipld::Map(g) = &got else { unreachable!() };
                let want_comps: BTreeMap<String, Ipld> = p
                    .components
                    .iter()
                    .map(|(ty, v)| Ok((vw.path(*ty)?, v.to_ipld()?)))
                    .collect::<R<_>>()?;
                let Ipld::Map(gc) = &g["components"] else {
                    unreachable!()
                };
                let got_comps: BTreeMap<String, Ipld> = gc
                    .iter()
                    .map(|(k, v)| match v {
                        Ipld::Map(m) => (k.clone(), m["value"].clone()),
                        _ => (k.clone(), Ipld::Null),
                    })
                    .collect();
                if got_comps != want_comps {
                    return Err(format!(
                        "verify: entity {}: its components read back differently from its record",
                        hex(&p.id)
                    ));
                }
                if list(&g["tags"]).len() != p.tags.len()
                    || list(&g["pairs"]).len() != p.pairs.len()
                    || g.contains_key("parent_component") != p.parent_component
                    || g.get("children").map(|c| list(c).len()) != p.children.as_ref().map(Vec::len)
                {
                    return Err(format!(
                        "verify: entity {}: its tags, pairs, parent storage or children read back differently \
                         from its record",
                        hex(&p.id)
                    ));
                }
                let _ = p.raw;
            }
        }
        Ok(())
    }

    /// Moves every value of type `from` to type `to` along a migration chain, in place, all or nothing (SPEC.md,
    /// "Live migration"): owners of `from`, `(from, *)` and `(*, from)`, prefabs included.
    pub fn migrate_live(&mut self, m: &Migrations, from: &str, to: &str) -> R<Report> {
        let w = self.w;
        // SAFETY: plain query.
        if unsafe { sys::glue_is_deferred(w) } {
            return Err(
                "the world is deferred (a stage, or in a system or observer): migrate outside one".into(),
            );
        }
        let (ft, tt) = (lookup(w, from)?, lookup(w, to)?);
        let mut walk = Walk::new(w, &self.s);
        let (fr, tr) = (walk.layout_record_of(ft)?, walk.layout_record_of(tt)?);
        let (fh, th) = (migrate::hash_of(&fr)?, migrate::hash_of(&tr)?);
        let chain = m
            .chain(&fh, &th)
            .ok_or_else(|| format!("no migration chain from {from} to {to}"))?;
        let tl = walk.layout(tt)?;
        let size = walk.component_size(tt)?;
        // SAFETY: a C global set by ecs_init.
        let wildcard = unsafe { sys::EcsWildcard };
        let mut report = Report::default();
        // (entity, old id, new id, converted value)
        let mut work: Vec<(ecs_entity_t, ecs_id_t, ecs_id_t, Ipld)> = Vec::new();
        let mut seen = HashSet::new();
        for q in [ft, sys::pair(ft, wildcard), sys::pair(wildcard, ft)] {
            let owners = grow(|buf, cap| {
                // SAFETY: `buf` has room for `cap` ids.
                unsafe { sys::glue_with(w, q, buf, cap) }
            });
            for e in owners {
                for id in type_ids(w, e) {
                    let new = if id == ft {
                        tt
                    } else if sys::is_pair(id) && sys::pair_first(id) == ft & 0xffff_ffff {
                        sys::pair(tt, sys::pair_second(id))
                    } else if sys::is_pair(id) && sys::pair_second(id) == ft & 0xffff_ffff {
                        sys::pair(sys::pair_first(id), tt)
                    } else {
                        continue;
                    };
                    // SAFETY: plain query.
                    if unsafe { sys::ecs_get_typeid(w, id) } != ft {
                        continue;
                    }
                    if !seen.insert((e, id)) {
                        continue;
                    }
                    // Named by id (or number) only on error: a path costs a children scan for an unnamed entity.
                    let what = || match self.s.stable_id(w, e) {
                        Some(id) => format!("entity {}", hex(&id)),
                        None => format!("entity #{}", e & 0xffff_ffff),
                    };
                    if self.has(e, new) {
                        return Err(format!(
                            "{}: already has {to}, so {from} cannot move to it",
                            what()
                        ));
                    }
                    // SAFETY: plain query; the value lives while the world is unchanged (nothing is written until
                    // every value is converted).
                    let ptr = unsafe { sys::ecs_get_id(w, e, id) }.cast::<u8>();
                    let v = walk
                        .read_value(ft, ptr)
                        .map_err(|m| format!("{}: {from}: {m}", what()))?;
                    let w2 = w;
                    let mut default = |rec: &Ipld| -> Option<Ipld> {
                        let p = migrate::path_of(rec).ok()?;
                        let ty = lookup(w2, &p).ok()?;
                        walk.default_value(ty).ok()
                    };
                    let nv = m
                        .convert_chain(&chain, &v, &mut default, &mut report)
                        .map_err(|m| format!("{}: {m}", what()))?;
                    work.push((e, id, new, nv));
                }
            }
        }
        let live: HashMap<[u8; 16], ecs_entity_t> = HashMap::new();
        let made = HashMap::new();
        let remint = HashMap::new();
        let mut ids = LiveIds {
            w,
            s: &self.s,
            inner: HostIds {
                w,
                made: &made,
                remint: &remint,
                live: &live,
            },
            index: None,
        };
        let mut added: Vec<(ecs_entity_t, ecs_id_t)> = Vec::new();
        for (e, _, new, v) in &work {
            let r = make_value(w, &self.s, tt, v, &tl, &mut ids).map(|val| {
                // SAFETY: `val` holds a value of `to`, `size` bytes.
                unsafe { sys::glue_writer_commit(w, val.0, *e, *new, size) };
            });
            if let Err(m) = r {
                for (e, id) in &added {
                    // SAFETY: parts this call added.
                    unsafe { sys::ecs_remove_id(w, *e, *id) };
                }
                return Err(format!("writing {to}: {m}"));
            }
            added.push((*e, *new));
        }
        for (e, old, _, _) in &work {
            // SAFETY: `old` is in `e`'s type.
            unsafe { sys::ecs_remove_id(w, *e, *old) };
        }
        Ok(report)
    }
}

/// Refs in a live world: ids through an index of every entity with the id component (built on first use).
struct LiveIds<'a> {
    w: *mut ecs_world_t,
    s: &'a Schema,
    inner: HostIds<'a>,
    index: Option<HashMap<[u8; 16], ecs_entity_t>>,
}

impl Resolve for LiveIds<'_> {
    fn entity(&mut self, r: &Ipld) -> R<ecs_entity_t> {
        if let Ipld::Map(m) = r
            && let Some(Ipld::Bytes(id)) = m.get("id")
        {
            let id: [u8; 16] = id.as_slice().try_into().map_err(|_| "id is not 16 bytes")?;
            let (w, s) = (self.w, self.s);
            let index = self.index.get_or_insert_with(|| {
                grow(|buf, cap| {
                    // SAFETY: `buf` has room for `cap` ids.
                    unsafe { sys::glue_with(w, s.id_component, buf, cap) }
                })
                .into_iter()
                .filter_map(|e| s.stable_id(w, e).map(|id| (id, e)))
                .collect()
            });
            return index
                .get(&id)
                .copied()
                .ok_or_else(|| format!("no entity has the id {}", hex(&id)));
        }
        self.inner.entity(r)
    }
}

/// What differs between two host roots (SPEC.md, "Host blocks"), by entity id: added, removed or changed. Equal MST
/// subtrees are read in full (the entity lists are compared), but entity blocks are never fetched.
pub fn diff(s: &dyn BlockStore, a: &Cid, b: &Cid) -> R<Vec<([u8; 16], Kind)>> {
    let root = |c: &Cid| -> R<(Cid, Ipld)> {
        let bytes = dag::fetch(s, c)?;
        let v: Ipld =
            serde_ipld_dagcbor::from_slice(&bytes).map_err(|e| format!("host root {c}: {e}"))?;
        let m = check::exact(&v, &["version", "ids", "layouts", "entities"], "host root")?;
        check::version(m, "the host root")?;
        check::layouts(m.get("layouts"))?;
        match &m["entities"] {
            Ipld::Link(l) => Ok((*l, m["layouts"].clone())),
            _ => Err("host root: entities is not a link".into()),
        }
    };
    let (ea, _) = root(a)?;
    let (eb, _) = root(b)?;
    if ea == eb {
        return Ok(Vec::new());
    }
    let ma: BTreeMap<Vec<u8>, Cid> = mst::read(s, &ea)?.into_iter().collect();
    let mb: BTreeMap<Vec<u8>, Cid> = mst::read(s, &eb)?.into_iter().collect();
    let mut out = Vec::new();
    let id = |k: &[u8]| -> R<[u8; 16]> {
        k.try_into()
            .map_err(|_| "an MST key is not 16 bytes".to_string())
    };
    for (k, c) in &ma {
        match mb.get(k) {
            None => out.push((id(k)?, Kind::Removed)),
            Some(d) if d != c => out.push((id(k)?, Kind::Changed)),
            _ => {}
        }
    }
    for k in mb.keys() {
        if !ma.contains_key(k) {
            out.push((id(k)?, Kind::Added));
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
