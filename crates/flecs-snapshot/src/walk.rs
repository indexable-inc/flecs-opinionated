//! Walks a world into snapshot records (SPEC.md, "Snapshot"): the entities under the roots, depth first, each with
//! its stable id, tags, pairs, component values and prefabs, plus the layout record of every type a value uses.
//! Entities are referred to by stable id, or by path when they have none (Flecs's builtins); never by Flecs id.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{CStr, c_char};

use flecs_c as sys;
use ipld_core::ipld::Ipld;
use sha2::{Digest, Sha256};
use sys::{ecs_entity_t, ecs_id_t, ecs_world_t};

use cid::Cid;

use crate::cbor;
use crate::dag::{self, DAG_CBOR, RAW};
use crate::merge::Rule;
use crate::mode::Mode;
use crate::schema::Schema;

/// Past this many children a parent's child list is a Merkle Search Tree (SPEC.md, "Large child sets").
pub const MAX_CHILDREN: usize = 4096;

/// Blob chunking (SPEC.md, "Blobs"): FastCDC 2020 (fastcdc 5.0.0, normalization level 1, seed 0) with these sizes.
pub const CHUNK_MIN: usize = 16 * 1024;
pub const CHUNK_AVG: usize = 64 * 1024;
pub const CHUNK_MAX: usize = 256 * 1024;
/// A blob's chunk list is one block; past this many chunks (about 1 GiB at the average size) a blob is refused.
pub const MAX_CHUNKS: usize = 16384;

/// The ids of Flecs's builtins (C globals, set once the first world exists).
struct Builtins {
    child_of: ecs_entity_t,
    is_a: ecs_entity_t,
    prefab: ecs_entity_t,
    ordered_children: ecs_entity_t,
    on_instantiate: ecs_entity_t,
    inherit: ecs_entity_t,
    dont_fragment: ecs_entity_t,
    wildcard: ecs_entity_t,
    exclusive: ecs_entity_t,
    parent_depth: ecs_entity_t,
    identifier: ecs_entity_t,
    parent: ecs_entity_t,
    component: ecs_entity_t,
    type_: ecs_entity_t,
    primitive: ecs_entity_t,
    struct_: ecs_entity_t,
    enum_: ecs_entity_t,
    constants: ecs_entity_t,
    array: ecs_entity_t,
    vector: ecs_entity_t,
}

impl Builtins {
    fn read() -> Builtins {
        // SAFETY: plain reads of C globals Flecs set when the first world was created (the caller holds a world).
        unsafe {
            Builtins {
                child_of: sys::EcsChildOf,
                is_a: sys::EcsIsA,
                prefab: sys::EcsPrefab,
                ordered_children: sys::EcsOrderedChildren,
                on_instantiate: sys::EcsOnInstantiate,
                inherit: sys::EcsInherit,
                dont_fragment: sys::EcsDontFragment,
                wildcard: sys::EcsWildcard,
                exclusive: sys::EcsExclusive,
                parent_depth: sys::EcsParentDepth,
                identifier: sys::FLECS_IDEcsIdentifierID_,
                parent: sys::FLECS_IDEcsParentID_,
                component: sys::FLECS_IDEcsComponentID_,
                type_: sys::FLECS_IDEcsTypeID_,
                primitive: sys::FLECS_IDEcsPrimitiveID_,
                struct_: sys::FLECS_IDEcsStructID_,
                enum_: sys::FLECS_IDEcsEnumID_,
                constants: sys::FLECS_IDEcsConstantsID_,
                array: sys::FLECS_IDEcsArrayID_,
                vector: sys::FLECS_IDEcsVectorID_,
            }
        }
    }
}

/// What one id in an entity's type becomes.
#[derive(Clone, Copy)]
enum Part {
    /// Not in the snapshot: the name, the parent, IsA (see `isa`), the stable id, ids with flags.
    Skip,
    Tag(ecs_entity_t),
    Component(ecs_entity_t),
    /// A pair, with its data type when it carries a value.
    Pair(ecs_entity_t, ecs_entity_t, Option<ecs_entity_t>),
}

/// One entity of the snapshot.
pub struct Item {
    pub entity: ecs_entity_t,
    pub parent: Option<usize>,
    pub path: String,
    /// The path segment under its parent (the name, escaped, or `#n`).
    pub segment: String,
    pub name: Option<String>,
    pub children: Vec<usize>,
    /// The entity's own fields (SPEC.md, "Entity record"), without path, parent or children.
    pub record: BTreeMap<String, Ipld>,
    /// Its Ephemeral parts (SPEC.md, "Sync modes"): `{"tags", "pairs", "components"}`, or None if it has none.
    pub ephemeral: Option<Ipld>,
}

pub struct Snapshot {
    pub items: Vec<Item>,
    pub roots: Vec<usize>,
    /// Type path to layout record.
    pub layouts: BTreeMap<String, Ipld>,
    /// The layout records of the Ephemeral parts' types.
    pub ephemeral_layouts: BTreeMap<String, Ipld>,
    /// Blob blocks (raw chunks and chunk lists), each once, in the order the walk first reached them.
    pub blobs: Vec<(Cid, Vec<u8>)>,
}

pub type R<T> = Result<T, String>;

fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: Flecs returns NUL-terminated strings that live as long as the entity.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// A path segment: `\` and `.` escaped with `\`, and a leading `#` too, so a name never reads as an unnamed child's
/// `#n`.
pub fn escape(name: &str) -> String {
    let e = name.replace('\\', "\\\\").replace('.', "\\.");
    if e.starts_with('#') {
        format!("\\{e}")
    } else {
        e
    }
}

/// Puts a member (or `[i]` element) in front of an error's member path: `member pos.x: ...`, `member pts[2].x: ...`.
pub(crate) fn member_err(name: &str, e: String) -> String {
    match e.strip_prefix("member ") {
        Some(rest) if rest.starts_with('[') => format!("member {name}{rest}"),
        Some(rest) => format!("member {name}.{rest}"),
        None => format!("member {name}: {e}"),
    }
}

fn text(s: impl Into<String>) -> Ipld {
    Ipld::String(s.into())
}

fn map<const N: usize>(entries: [(&str, Ipld); N]) -> Ipld {
    Ipld::Map(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

/// DAG-CBOR bytes of a value built here (always encodable: no NaN, no infinities).
pub fn encode(v: &Ipld) -> Vec<u8> {
    serde_ipld_dagcbor::to_vec(v).expect("snapshot values are valid DAG-CBOR")
}

/// The first 8 bytes of SHA-256: a layout's hash.
pub fn hash8(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes)[..8].to_vec()
}

/// Sorts values by their DAG-CBOR encoding, so order never depends on Flecs ids.
fn sort_encoded(v: &mut [Ipld]) {
    v.sort_by_cached_key(encode);
}

/// A host world's walk (SPEC.md, "Host worlds"): which entities are saved and how refs to others are written.
/// A value in a host record before it is read: (type path, layout hash, type, pointer to the value).
type ValuePart = (String, Ipld, ecs_entity_t, *const u8);

/// A host record's parts with its values unread (`Walk::host_parts`).
struct HostParts {
    id: [u8; 16],
    name: Option<String>,
    parent: Ipld,
    tags: Vec<Ipld>,
    pairs: Vec<(Ipld, Ipld, Option<ValuePart>)>,
    components: Vec<ValuePart>,
    isa: Vec<Ipld>,
}

pub struct HostCfg {
    /// The select tag: not a part of any record.
    pub select: ecs_entity_t,
    /// The saved entities.
    pub saved: HashSet<ecs_entity_t>,
    /// Refs to an entity outside `saved` that has an id are written as `{"id"}` instead of refused.
    pub outside_by_id: bool,
    /// Which named entities may be written as `{"path"}` (`crate::host::Naming`; none: any named one).
    pub naming: Option<crate::host::Naming>,
}

pub struct Walk<'s> {
    w: *const ecs_world_t,
    host: Option<HostCfg>,
    /// The component and pair types whose layouts the current record used.
    pub used: HashSet<ecs_entity_t>,
    b: Builtins,
    s: &'s Schema,
    items: Vec<Item>,
    index: HashMap<ecs_entity_t, usize>,
    paths: HashMap<ecs_entity_t, String>,
    layouts: BTreeMap<String, Ipld>,
    ephemeral_layouts: BTreeMap<String, Ipld>,
    blobs: Vec<(Cid, Vec<u8>)>,
    blob_seen: HashSet<Cid>,
    /// Set while reading an Ephemeral value, where a blob (which needs stored blocks) is refused.
    in_ephemeral: bool,
    /// Stable id to the path of the entity that has it, to refuse two entities with one id.
    stable: HashMap<[u8; 16], String>,
    /// User types marked DontFragment: their values live outside the entity's type, where the walk does not read
    /// them, so an entity that has one is refused rather than written without it.
    dont_fragment: Vec<ecs_entity_t>,
    /// Per-type caches, valid for one walk (a walk never sees a type change): members, paths for messages, layouts.
    members_cache: HashMap<ecs_entity_t, std::rc::Rc<[Member]>>,
    names_cache: HashMap<ecs_entity_t, std::rc::Rc<str>>,
    layout_cache: HashMap<ecs_entity_t, Ipld>,
    /// A struct's members in DAG-CBOR key order, each with its encoded key.
    sorted_cache: HashMap<ecs_entity_t, Sorted>,
    /// Per type: how `write_value` writes it.
    kinds: HashMap<ecs_entity_t, WriteKind>,
}

/// A struct's members in DAG-CBOR key order, each with its encoded key.
type Sorted = std::rc::Rc<[(Vec<u8>, Member)]>;

/// How `write_value` writes a type's values.
#[derive(Clone, Copy)]
enum WriteKind {
    Primitive(i32),
    Struct,
    /// An inline array: element type, count.
    Array(ecs_entity_t, usize),
    Vector(ecs_entity_t),
    Bytes,
    /// Anything else: read into an `Ipld` and written from that.
    Other,
}

impl<'s> Walk<'s> {
    pub fn new(w: *const ecs_world_t, s: &'s Schema) -> Walk<'s> {
        Walk {
            w,
            host: None,
            used: HashSet::new(),
            b: Builtins::read(),
            s,
            items: Vec::new(),
            index: HashMap::new(),
            paths: HashMap::new(),
            layouts: BTreeMap::new(),
            ephemeral_layouts: BTreeMap::new(),
            blobs: Vec::new(),
            blob_seen: HashSet::new(),
            in_ephemeral: false,
            stable: HashMap::new(),
            dont_fragment: Vec::new(),
            members_cache: HashMap::new(),
            names_cache: HashMap::new(),
            layout_cache: HashMap::new(),
            sorted_cache: HashMap::new(),
            kinds: HashMap::new(),
        }
    }

    pub fn new_host(w: *const ecs_world_t, s: &'s Schema, cfg: HostCfg) -> Walk<'s> {
        let mut walk = Walk::new(w, s);
        walk.host = Some(cfg);
        walk
    }

    fn name(&self, e: ecs_entity_t) -> Option<String> {
        // SAFETY: `e` is alive in `w`.
        cstr(unsafe { sys::ecs_get_name(self.w, e) })
    }

    fn parent(&self, e: ecs_entity_t) -> ecs_entity_t {
        // SAFETY: `e` is alive in `w`.
        unsafe { sys::ecs_get_parent(self.w, e) }
    }

    fn has(&self, e: ecs_entity_t, id: ecs_id_t) -> bool {
        // SAFETY: `e` is alive in `w`.
        unsafe { sys::ecs_has_id(self.w, e, id) }
    }

    fn target(&self, e: ecs_entity_t, rel: ecs_entity_t) -> ecs_entity_t {
        // SAFETY: plain query.
        unsafe { sys::ecs_get_target(self.w, e, rel, 0) }
    }

    fn get<T>(&self, e: ecs_entity_t, id: ecs_id_t) -> Option<&T> {
        // SAFETY: `id` is the component T mirrors; the value lives while the world is unchanged (it is, during a walk).
        unsafe { sys::ecs_get_id(self.w, e, id).cast::<T>().as_ref() }
    }

    fn ids(&self, e: ecs_entity_t) -> Vec<ecs_id_t> {
        // SAFETY: `e` is alive; the type array has `count` ids.
        unsafe {
            let t = sys::ecs_get_type(self.w, e);
            if t.is_null() || (*t).count == 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts((*t).array, (*t).count as usize).to_vec()
        }
    }

    fn alive(&self, e: ecs_entity_t) -> ecs_entity_t {
        // SAFETY: plain lookup.
        unsafe { sys::ecs_get_alive(self.w, e) }
    }

    /// The children as Flecs lists them: declared order under OrderedChildren, otherwise sorted by id.
    fn raw_children(&self, e: ecs_entity_t) -> Vec<ecs_entity_t> {
        let mut out = crate::world::grow(|buf, cap| {
            // SAFETY: `buf` has room for `cap` ids.
            unsafe { sys::glue_children(self.w, e, buf, cap) }
        });
        if !self.has(e, self.b.ordered_children) {
            out.sort_unstable();
        }
        out
    }

    /// The path segment of each child, and the children in snapshot order: declared order under OrderedChildren,
    /// otherwise by segment (so the order never depends on which entity Flecs created first). Unnamed children are
    /// `#n`, numbered in id order among the unnamed ones.
    pub fn children(&self, e: ecs_entity_t) -> Vec<(String, ecs_entity_t)> {
        let ordered = self.has(e, self.b.ordered_children);
        let mut unnamed = 0;
        let mut kids: Vec<(String, ecs_entity_t)> = Vec::new();
        let mut raw = self.raw_children(e);
        if ordered {
            // Number unnamed children in id order even when listed in declared order.
            let mut by_id = raw.clone();
            by_id.sort_unstable();
            let ordinal: HashMap<_, _> = by_id
                .iter()
                .filter(|c| self.name(**c).is_none())
                .enumerate()
                .map(|(i, c)| (*c, i))
                .collect();
            for c in raw {
                let seg = self
                    .name(c)
                    .map_or_else(|| format!("#{}", ordinal[&c]), |n| escape(&n));
                kids.push((seg, c));
            }
            return kids;
        }
        raw.sort_unstable();
        for c in raw {
            let seg = match self.name(c) {
                Some(n) => escape(&n),
                None => {
                    unnamed += 1;
                    format!("#{}", unnamed - 1)
                }
            };
            kids.push((seg, c));
        }
        kids.sort();
        kids
    }

    /// The entity at a snapshot path: segments split at unescaped `.`, each matched exactly against the segments
    /// `children` gives (so `#n` is an unnamed child and `\\#n` a named one). Never searches parent scopes the way
    /// `ecs_lookup` does.
    pub fn resolve(&self, path: &str) -> R<ecs_entity_t> {
        let mut segs = Vec::new();
        let mut cur = String::new();
        let mut it = path.chars();
        while let Some(ch) = it.next() {
            match ch {
                '\\' => {
                    cur.push(ch);
                    cur.push(
                        it.next()
                            .ok_or_else(|| format!("path {path:?} ends in a lone \\"))?,
                    );
                }
                '.' => segs.push(std::mem::take(&mut cur)),
                _ => cur.push(ch),
            }
        }
        segs.push(cur);
        let missing = || format!("no entity named {path}");
        let first = &segs[0];
        let roots = crate::world::grow(|buf, cap| {
            // SAFETY: `buf` has room for `cap` ids.
            unsafe { sys::glue_roots(self.w.cast_mut(), buf, cap) }
        });
        let mut e = roots
            .into_iter()
            .find(|r| self.name(*r).is_some_and(|n| escape(&n) == *first))
            .ok_or_else(missing)?;
        for seg in &segs[1..] {
            e = self
                .children(e)
                .into_iter()
                .find(|(s, _)| s == seg)
                .map(|(_, c)| c)
                .ok_or_else(missing)?;
        }
        Ok(e)
    }

    /// The path of any entity (entities in the snapshot have theirs already).
    pub fn path(&mut self, e: ecs_entity_t) -> R<String> {
        if let Some(i) = self.index.get(&e) {
            return Ok(self.items[*i].path.clone());
        }
        if let Some(p) = self.paths.get(&e) {
            return Ok(p.clone());
        }
        // SAFETY: plain query.
        if !unsafe { sys::ecs_is_alive(self.w, e) } {
            return Err(format!("refers to a deleted entity (#{})", e & 0xffff_ffff));
        }
        let parent = self.parent(e);
        if self.host.is_some() && self.name(e).is_none() {
            return Err(format!(
                "entity #{} has no name, so it has no path that means the same in another world (and no id)",
                e & 0xffff_ffff
            ));
        }
        let p = if let (true, Some(n)) = (parent != 0, self.name(e)) {
            // A named child's segment is its escaped name (as `children` gives it), so the siblings need not be listed.
            format!("{}.{}", self.path(parent)?, escape(&n))
        } else if parent == 0 {
            match self.name(e) {
                Some(n) => escape(&n),
                None => {
                    return Err(format!(
                        "entity #{} has no name and no parent, so it has no path",
                        e & 0xffff_ffff
                    ));
                }
            }
        } else {
            let seg = self
                .children(parent)
                .into_iter()
                .find(|(_, c)| *c == e)
                .map(|(s, _)| s)
                .ok_or("child not found under its parent")?;
            format!("{}.{seg}", self.path(parent)?)
        };
        self.paths.insert(e, p.clone());
        Ok(p)
    }

    fn is_builtin(&self, mut e: ecs_entity_t) -> bool {
        loop {
            let p = self.parent(e);
            if p == 0 {
                return self.name(e).as_deref() == Some("flecs");
            }
            e = p;
        }
    }

    /// A reference to an entity: `{"id": bytes}` when it has a stable id, else `{"path": text}`.
    fn reference(&mut self, e: ecs_entity_t) -> R<Ipld> {
        if let Some(id) = self.s.stable_id(self.w, e) {
            if let Some(h) = &self.host
                && !h.outside_by_id
                && !h.saved.contains(&e)
            {
                let what = self
                    .path(e)
                    .unwrap_or_else(|_| format!("#{}", e & 0xffff_ffff));
                return Err(format!(
                    "refers to {what} (id {}), which has an id but is not saved: save it too, or allow refs by id \
                     to entities outside the selection (Outside::ById)",
                    hex(&id)
                ));
            }
            return Ok(map([("id", Ipld::Bytes(id.to_vec()))]));
        }
        let path = self.path(e)?;
        if let Some(naming) = self.host.as_ref().and_then(|h| h.naming.as_ref()) {
            self.authored(e, &path, naming)?;
        }
        Ok(map([("path", text(path))]))
    }

    /// Refuses a ref by `path` to `e` unless `e` is made at startup whatever a document holds: no step of its path
    /// an instance made at run time (an `IsA` that is not a prefab), none under one of `naming`'s run-time scopes,
    /// its top not a run-time root (unless a step is a prefab), and the path looks `e` up again.
    fn authored(&self, e: ecs_entity_t, path: &str, naming: &crate::host::Naming) -> R<()> {
        let refused = |why: String| {
            Err(format!(
                "refers to {path}, which has no id and {why}, so its path would not find it in the next \
                 process: save it too (the select tag), or refer to an authored entity"
            ))
        };
        let mut prefab = false;
        let mut at = e;
        let mut top = e;
        while at != 0 {
            let name = self.name(at).unwrap_or_default();
            let is_prefab = self.has(at, self.b.prefab);
            prefab |= is_prefab;
            if !is_prefab && self.has(at, sys::pair(self.b.is_a, self.b.wildcard)) {
                return refused(format!(
                    "is (or is under) {name}, an instance made at run time"
                ));
            }
            if at != e && naming.runtime_scopes.contains(&name) {
                return refused(format!("is under {name}, a scope made at run time"));
            }
            top = at;
            at = self.parent(at);
        }
        let root = self.name(top).unwrap_or_default();
        if !prefab && naming.runtime_roots.contains(&root) {
            return refused(format!("is under {root}, a root made at run time"));
        }
        match crate::host::lookup(self.w, path) {
            Ok(found) if found == e => Ok(()),
            _ => refused("its path finds another entity or none".to_string()),
        }
    }

    /// The user types marked DontFragment, found once per walk.
    pub fn find_dont_fragment(&mut self) {
        let df = self.b.dont_fragment;
        let all = crate::world::grow(|buf, cap| {
            // SAFETY: `buf` has room for `cap` ids.
            unsafe { sys::glue_with(self.w, df, buf, cap) }
        });
        self.dont_fragment = all.into_iter().filter(|t| !self.is_builtin(*t)).collect();
    }

    /// A host record (SPEC.md, "Host record") of a saved entity: its own parts only, its direct bases, its parent and
    /// name. `used` holds the types whose layouts it used.
    pub fn host_record(&mut self, e: ecs_entity_t) -> R<BTreeMap<String, Ipld>> {
        let p = self.host_parts(e)?;
        let mut components = BTreeMap::new();
        for (path, hash, ty, ptr) in p.components {
            let value = self
                .read_value(ty, ptr)
                .map_err(|m| format!("component {path}: {m}"))?;
            components.insert(path, map([("hash", hash), ("value", value)]));
        }
        let mut pairs = Vec::with_capacity(p.pairs.len());
        for (r, t, data) in p.pairs {
            let mut entry = vec![r, t];
            if let Some((tp, hash, ty, ptr)) = data {
                let value = self
                    .read_value(ty, ptr)
                    .map_err(|m| format!("pair with data type {tp}: {m}"))?;
                entry.push(map([("type", text(tp)), ("hash", hash), ("value", value)]));
            }
            pairs.push(Ipld::List(entry));
        }
        sort_encoded(&mut pairs);
        let mut rec = BTreeMap::new();
        rec.insert("id".into(), Ipld::Bytes(p.id.to_vec()));
        rec.insert("name".into(), p.name.map_or(Ipld::Null, Ipld::String));
        rec.insert("parent".into(), p.parent);
        rec.insert("tags".into(), Ipld::List(p.tags));
        rec.insert("pairs".into(), Ipld::List(pairs));
        rec.insert("components".into(), Ipld::Map(components));
        rec.insert("isa".into(), Ipld::List(p.isa));
        Ok(rec)
    }

    /// The host record of `e` as DAG-CBOR, written straight from the entity's memory: the same bytes as
    /// `encode(&host_record(e))`, with no `Ipld` tree for the values.
    pub fn host_record_cbor(&mut self, e: ecs_entity_t) -> R<([u8; 16], Vec<u8>)> {
        let p = self.host_parts(e)?;
        let mut out = Vec::with_capacity(256);
        cbor::map(&mut out, 7);
        // DAG-CBOR key order: id, isa, name, tags, pairs, parent, components.
        cbor::text(&mut out, "id");
        cbor::bytes(&mut out, &p.id);
        cbor::text(&mut out, "isa");
        cbor::ipld(&mut out, &Ipld::List(p.isa));
        cbor::text(&mut out, "name");
        match &p.name {
            Some(n) => cbor::text(&mut out, n),
            None => cbor::null(&mut out),
        }
        cbor::text(&mut out, "tags");
        cbor::ipld(&mut out, &Ipld::List(p.tags));
        cbor::text(&mut out, "pairs");
        let mut pairs = Vec::with_capacity(p.pairs.len());
        for (r, t, data) in p.pairs {
            let mut b = Vec::new();
            cbor::list(&mut b, if data.is_some() { 3 } else { 2 });
            cbor::ipld(&mut b, &r);
            cbor::ipld(&mut b, &t);
            if let Some((tp, hash, ty, ptr)) = data {
                cbor::map(&mut b, 3);
                cbor::text(&mut b, "hash");
                cbor::ipld(&mut b, &hash);
                cbor::text(&mut b, "type");
                cbor::text(&mut b, &tp);
                cbor::text(&mut b, "value");
                self.write_value(ty, ptr, &mut b)
                    .map_err(|m| format!("pair with data type {tp}: {m}"))?;
            }
            pairs.push(b);
        }
        pairs.sort();
        cbor::list(&mut out, pairs.len());
        for b in pairs {
            out.extend_from_slice(&b);
        }
        cbor::text(&mut out, "parent");
        cbor::ipld(&mut out, &p.parent);
        cbor::text(&mut out, "components");
        let mut comps = p.components;
        comps.sort_by(|a, b| cbor::key_order(&a.0, &b.0));
        cbor::map(&mut out, comps.len());
        for (path, hash, ty, ptr) in comps {
            cbor::text(&mut out, &path);
            cbor::map(&mut out, 2);
            cbor::text(&mut out, "hash");
            cbor::ipld(&mut out, &hash);
            cbor::text(&mut out, "value");
            self.write_value(ty, ptr, &mut out)
                .map_err(|m| format!("component {path}: {m}"))?;
        }
        Ok((p.id, out))
    }

    /// A host entity's record without its values: everything `host_record` checks and refuses, with each value as
    /// (path, layout hash, type, pointer).
    fn host_parts(&mut self, e: ecs_entity_t) -> R<HostParts> {
        self.used.clear();
        let id = self
            .s
            .stable_id(self.w, e)
            .ok_or("it has no id component")?;
        for i in 0..self.dont_fragment.len() {
            let t = self.dont_fragment[i];
            if self.has(e, t) || self.has(e, sys::pair(t, self.b.wildcard)) {
                return Err(format!(
                    "{} is DontFragment, which snapshots do not support yet",
                    self.path(t)?
                ));
            }
        }
        let select = self.host.as_ref().map_or(0, |h| h.select);
        // SAFETY: a C global set by ecs_init.
        let toggle = unsafe { sys::ECS_TOGGLE };
        let mut tags = Vec::new();
        let mut pairs = Vec::new();
        let mut components = Vec::new();
        let mut isa = Vec::new();
        for id in self.ids(e) {
            if id & toggle != 0 && !sys::is_pair(id) {
                let c = id & sys::ECS_COMPONENT_MASK;
                // SAFETY: plain query.
                if !unsafe { sys::ecs_is_enabled_id(self.w, e, c) } {
                    return Err(format!(
                        "its {} is disabled (CanToggle): a snapshot does not hold the enabled bit yet",
                        self.path(c)?
                    ));
                }
                continue;
            }
            if id == self.b.parent {
                return Err(
                    "it is in Parent storage, whose value is an entity id: not saved yet".into(),
                );
            }
            if id == select {
                continue;
            }
            if sys::is_pair(id) && self.alive(sys::pair_first(id)) == self.b.is_a {
                let base = self.alive(sys::pair_second(id));
                isa.push(self.reference(base)?);
                continue;
            }
            if sys::is_pair(id)
                && (sys::pair_first(id) == self.b.wildcard & 0xffff_ffff
                    || sys::pair_second(id) == self.b.wildcard & 0xffff_ffff)
            {
                return Err("it has a wildcard id".into());
            }
            let part = self.part(id);
            if matches!(part, Part::Skip) || self.ephemeral(part)? {
                continue;
            }
            // SAFETY: plain query; the pointer is valid while the world is unchanged.
            let ptr = unsafe { sys::ecs_get_id(self.w, e, id) }.cast::<u8>();
            match part {
                Part::Skip => {}
                Part::Tag(t) => tags.push(self.reference(t)?),
                Part::Component(c) => {
                    let p = self.path(c)?;
                    if ptr.is_null() {
                        return Err(format!("component {p}: no value"));
                    }
                    let hash = self
                        .layout_record(c, false)
                        .map_err(|m| format!("component {p}: {m}"))?;
                    components.push((p, hash, c, ptr));
                }
                Part::Pair(rel, tgt, ty) => {
                    let (r, t) = (self.reference(rel)?, self.reference(tgt)?);
                    let data = match ty {
                        Some(ty) => {
                            let tp = self.path(ty)?;
                            if ptr.is_null() {
                                return Err(format!("pair with data type {tp}: no value"));
                            }
                            let hash = self
                                .layout_record(ty, false)
                                .map_err(|m| format!("pair with data type {tp}: {m}"))?;
                            Some((tp, hash, ty, ptr))
                        }
                        None => None,
                    };
                    pairs.push((r, t, data));
                }
            }
        }
        sort_encoded(&mut tags);
        sort_encoded(&mut isa);
        let parent = match self.parent(e) {
            0 => Ipld::Null,
            p => self.reference(p).map_err(|m| format!("its parent: {m}"))?,
        };
        Ok(HostParts {
            id,
            name: self.name(e),
            parent,
            tags,
            pairs,
            components,
            isa,
        })
    }

    /// Whether `id` in a host entity's type is a part its host record lists (a tag, component or pair that is not
    /// skipped, not Ephemeral, not IsA and not the select tag). A restore removes base-copied parts of this kind that
    /// the record does not list.
    pub fn listed_part(&mut self, id: ecs_id_t) -> R<bool> {
        let select = self.host.as_ref().map_or(0, |h| h.select);
        if id == select || id == self.b.parent {
            return Ok(false);
        }
        if sys::is_pair(id) && self.alive(sys::pair_first(id)) == self.b.is_a {
            return Ok(false);
        }
        let part = self.part(id);
        Ok(!matches!(part, Part::Skip) && !self.ephemeral(part)?)
    }

    /// The layout records computed so far, by type path.
    pub fn take_layouts(&mut self) -> BTreeMap<String, Ipld> {
        std::mem::take(&mut self.layouts)
    }

    /// The layout records the last record used (`used`), by type path, as `take_layouts` will hold them.
    pub fn used_layouts(&mut self) -> R<BTreeMap<String, Ipld>> {
        let mut out = BTreeMap::new();
        let used: Vec<ecs_entity_t> = self.used.iter().copied().collect();
        for ty in used {
            let tp = self.path(ty)?;
            if let Some(rec) = self.layouts.get(&tp) {
                out.insert(tp, rec.clone());
            }
        }
        Ok(out)
    }

    /// The layout record of a type (computing it if this walk has not), as `layouts` will hold it.
    pub fn layout_record_of(&mut self, ty: ecs_entity_t) -> R<Ipld> {
        self.layout_record(ty, false)?;
        let tp = self.path(ty)?;
        Ok(self.layouts[&tp].clone())
    }

    fn collect(
        &mut self,
        e: ecs_entity_t,
        parent: Option<usize>,
        segment: String,
        path: String,
    ) -> R<usize> {
        if self.index.contains_key(&e) {
            return Err(format!("{path} is reached twice: the roots overlap"));
        }
        if let Some(id) = self.s.stable_id(self.w, e)
            && let Some(other) = self.stable.insert(id, path.clone())
        {
            return Err(format!("{other} and {path} have the same stable id"));
        }
        let i = self.items.len();
        let name = self.name(e);
        self.items.push(Item {
            entity: e,
            parent,
            path: path.clone(),
            segment,
            name,
            children: Vec::new(),
            record: BTreeMap::new(),
            ephemeral: None,
        });
        self.index.insert(e, i);
        let kids = self.children(e);
        if kids.len() > MAX_CHILDREN && self.has(e, self.b.ordered_children) {
            return Err(format!(
                "{path} has {} children under OrderedChildren; past {MAX_CHILDREN} the child list is a Merkle \
                 Search Tree, which is ordered by name, so it cannot keep the declared order",
                kids.len()
            ));
        }
        for (seg, c) in kids {
            let child = self.collect(c, Some(i), seg.clone(), format!("{path}.{seg}"))?;
            self.items[i].children.push(child);
        }
        Ok(i)
    }

    fn part(&self, id: ecs_id_t) -> Part {
        let b = &self.b;
        if sys::is_pair(id) {
            let rel = self.alive(sys::pair_first(id));
            let tgt = self.alive(sys::pair_second(id));
            if rel == 0
                || tgt == 0
                || [b.child_of, b.is_a, b.identifier, b.parent_depth].contains(&rel)
            {
                return Part::Skip;
            }
            // SAFETY: plain query.
            let ty = unsafe { sys::ecs_get_typeid(self.w, id) };
            let ty = (ty != 0 && !self.is_builtin(ty)).then_some(ty);
            return Part::Pair(rel, tgt, ty);
        }
        if id & sys::ECS_ID_FLAGS_MASK != 0 || id == b.parent || id == self.s.id_component {
            return Part::Skip;
        }
        // SAFETY: plain query.
        let ty = unsafe { sys::ecs_get_typeid(self.w, id) };
        if ty == 0 || self.is_builtin(id) {
            Part::Tag(id)
        } else {
            Part::Component(id)
        }
    }

    /// Every prefab `e` is an instance of, nearest first (bases at one depth in path order).
    fn isa(&mut self, e: ecs_entity_t) -> R<Vec<ecs_entity_t>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut layer = vec![e];
        while !layer.is_empty() {
            // The whole layer (every base at this depth) is sorted by path, not each entity's bases on their own.
            let mut bases = Vec::new();
            for x in layer {
                for id in self.ids(x) {
                    if sys::is_pair(id) && self.alive(sys::pair_first(id)) == self.b.is_a {
                        let base = self.alive(sys::pair_second(id));
                        if seen.insert(base) {
                            bases.push((self.path(base)?, base));
                        }
                    }
                }
            }
            bases.sort();
            let next: Vec<_> = bases.into_iter().map(|(_, b)| b).collect();
            out.extend(&next);
            layer = next;
        }
        Ok(out)
    }

    pub fn run(mut self, roots: &[&str]) -> R<Snapshot> {
        if roots.is_empty() {
            return Err("a snapshot needs at least one root".into());
        }
        let mut root_items = Vec::new();
        for root in roots {
            let e = self.resolve(root)?;
            if root_items.is_empty() {
                self.find_dont_fragment();
            }
            let path = self.path(e)?;
            let seg = path.clone();
            root_items.push(self.collect(e, None, seg, path)?);
        }
        for i in 0..self.items.len() {
            let (rec, eph) = self
                .record(i)
                .map_err(|m| format!("{}: {m}", self.items[i].path))?;
            self.items[i].record = rec;
            self.items[i].ephemeral = eph;
        }
        Ok(Snapshot {
            items: self.items,
            roots: root_items,
            layouts: self.layouts,
            ephemeral_layouts: self.ephemeral_layouts,
            blobs: self.blobs,
        })
    }

    /// The sync mode a type declares with `(Sync, Mode)`.
    fn mode_of(&mut self, e: ecs_entity_t) -> R<Mode> {
        let t = self.target(e, self.s.sync);
        if t == 0 {
            return Ok(Mode::DEFAULT);
        }
        match self.s.mode(t) {
            Some(m) => Ok(m),
            None => Err(format!("(Sync, {}) is not a sync mode", self.path(t)?)),
        }
    }

    /// Whether a part is Ephemeral: its tag or component type is, or a pair's relationship or data type is.
    fn ephemeral(&mut self, part: Part) -> R<bool> {
        Ok(match part {
            Part::Skip => false,
            Part::Tag(t) => self.mode_of(t)? == Mode::Ephemeral,
            Part::Component(c) => self.mode_of(c)? == Mode::Ephemeral,
            Part::Pair(rel, _, ty) => {
                self.mode_of(rel)? == Mode::Ephemeral
                    || match ty {
                        Some(ty) => self.mode_of(ty)? == Mode::Ephemeral,
                        None => false,
                    }
            }
        })
    }

    /// The entity's record, and its Ephemeral parts (kept out of the record) if it has any.
    fn record(&mut self, i: usize) -> R<(BTreeMap<String, Ipld>, Option<Ipld>)> {
        let e = self.items[i].entity;
        let mut tags = Vec::new();
        let mut pairs = Vec::new();
        let mut components = BTreeMap::new();
        let (mut eph_tags, mut eph_pairs, mut eph_comps) =
            (Vec::new(), Vec::new(), BTreeMap::new());
        let (mut inh_tags, mut inh_comps, mut inh_pairs) = (Vec::new(), Vec::new(), Vec::new());
        let mut seen: HashSet<ecs_id_t> = HashSet::new();
        let mut seen_rels: HashSet<ecs_entity_t> = HashSet::new();

        for t in self.dont_fragment.clone() {
            if self.has(e, t) || self.has(e, sys::pair(t, self.b.wildcard)) {
                return Err(format!(
                    "{} is DontFragment, which snapshots do not support yet",
                    self.path(t)?
                ));
            }
        }
        let isa = self.isa(e)?;
        let sources: Vec<_> = std::iter::once(e).chain(isa.iter().copied()).collect();
        for (depth, src) in sources.into_iter().enumerate() {
            let own = depth == 0;
            for id in self.ids(src) {
                let part = self.part(id);
                if matches!(part, Part::Skip) || seen.contains(&id) {
                    continue;
                }
                if !own {
                    if id == self.b.prefab {
                        continue;
                    }
                    let comp = if sys::is_pair(id) {
                        self.alive(sys::pair_first(id))
                    } else {
                        id
                    };
                    // Flecs finds a base's part through IsA only when it is (OnInstantiate, Inherit); the default
                    // (Override) copies it onto the instance, so the instance owns it.
                    if !self.has(comp, sys::pair(self.b.on_instantiate, self.b.inherit)) {
                        continue;
                    }
                    if let Part::Pair(rel, ..) = part
                        && seen_rels.contains(&rel)
                        && self.has(rel, self.b.exclusive)
                    {
                        continue;
                    }
                }
                seen.insert(id);
                let eph = self.ephemeral(part)?;
                self.in_ephemeral = eph;
                // SAFETY: plain query; the pointer is valid while the world is unchanged.
                let ptr = unsafe { sys::ecs_get_id(self.w, src, id) }.cast::<u8>();
                match part {
                    Part::Skip => {}
                    Part::Tag(t) => {
                        let r = self.reference(t)?;
                        if eph {
                            eph_tags.push(r);
                            continue;
                        }
                        if !own {
                            inh_tags.push(r.clone());
                        }
                        tags.push(r);
                    }
                    Part::Component(c) => {
                        let p = self.path(c)?;
                        let (hash, value) = self
                            .value(c, ptr, eph)
                            .map_err(|m| format!("component {p}: {m}"))?;
                        let v = map([("hash", hash), ("value", value)]);
                        if eph {
                            eph_comps.insert(p, v);
                            continue;
                        }
                        if !own {
                            inh_comps.push(text(&p));
                        }
                        components.insert(p, v);
                    }
                    Part::Pair(rel, tgt, ty) => {
                        seen_rels.insert(rel);
                        let (r, t) = (self.reference(rel)?, self.reference(tgt)?);
                        let mut entry = vec![r.clone(), t.clone()];
                        if let Some(ty) = ty {
                            let tp = self.path(ty)?;
                            let (hash, value) = self
                                .value(ty, ptr, eph)
                                .map_err(|m| format!("pair with data type {tp}: {m}"))?;
                            entry.push(map([("type", text(tp)), ("hash", hash), ("value", value)]));
                        }
                        if eph {
                            eph_pairs.push(Ipld::List(entry));
                            continue;
                        }
                        if !own {
                            inh_pairs.push(Ipld::List(vec![r, t]));
                        }
                        pairs.push(Ipld::List(entry));
                    }
                }
            }
        }
        self.in_ephemeral = false;
        for v in [
            &mut tags,
            &mut pairs,
            &mut inh_tags,
            &mut inh_comps,
            &mut inh_pairs,
            &mut eph_tags,
            &mut eph_pairs,
        ] {
            sort_encoded(v);
        }
        let isa = isa
            .into_iter()
            .map(|b| self.reference(b))
            .collect::<R<Vec<_>>>()?;
        let id = self
            .s
            .stable_id(self.w, e)
            .map_or(Ipld::Null, |id| Ipld::Bytes(id.to_vec()));
        let name = self.items[i].name.clone().map_or(Ipld::Null, Ipld::String);
        let ephemeral = (!eph_tags.is_empty() || !eph_pairs.is_empty() || !eph_comps.is_empty())
            .then(|| {
                map([
                    ("tags", Ipld::List(eph_tags)),
                    ("pairs", Ipld::List(eph_pairs)),
                    ("components", Ipld::Map(eph_comps)),
                ])
            });
        let mut rec = BTreeMap::new();
        rec.insert("id".into(), id);
        rec.insert("name".into(), name);
        rec.insert("tags".into(), Ipld::List(tags));
        rec.insert("pairs".into(), Ipld::List(pairs));
        rec.insert("components".into(), Ipld::Map(components));
        rec.insert("isa".into(), Ipld::List(isa));
        rec.insert(
            "inherited".into(),
            map([
                ("tags", Ipld::List(inh_tags)),
                ("components", Ipld::List(inh_comps)),
                ("pairs", Ipld::List(inh_pairs)),
            ]),
        );
        Ok((rec, ephemeral))
    }

    /// The size of a component type's values, for writing one.
    pub fn component_size(&self, ty: ecs_entity_t) -> R<usize> {
        self.size(ty)
    }

    /// The value at `ptr`, a live value of type `ty`, in its snapshot shape.
    pub fn read_value(&mut self, ty: ecs_entity_t, ptr: *const u8) -> R<Ipld> {
        if ptr.is_null() {
            return Err("no value".into());
        }
        self.read(ty, ptr)
    }

    fn write_kind(&mut self, ty: ecs_entity_t) -> WriteKind {
        if let Some(k) = self.kinds.get(&ty) {
            return *k;
        }
        let k = if ty == self.s.bytes {
            WriteKind::Bytes
        } else if ty == self.s.blob || self.described(ty).is_some() || self.refined(ty) {
            // A refined value (SPEC.md, "Logical types") is read whole and checked against its rules.
            WriteKind::Other
        } else {
            match self.get::<sys::EcsType>(ty, self.b.type_).map(|t| t.kind) {
                Some(sys::EcsPrimitiveType) => {
                    match self.get::<sys::EcsPrimitive>(ty, self.b.primitive) {
                        Some(p) if p.kind != sys::EcsId => WriteKind::Primitive(p.kind),
                        _ => WriteKind::Other,
                    }
                }
                Some(sys::EcsStructType) => WriteKind::Struct,
                Some(sys::EcsArrayType) => match self.get::<sys::EcsArray>(ty, self.b.array) {
                    Some(a) => WriteKind::Array(a.type_, a.count as usize),
                    None => WriteKind::Other,
                },
                Some(sys::EcsVectorType) => match self.get::<sys::EcsVector>(ty, self.b.vector) {
                    Some(v) => WriteKind::Vector(v.type_),
                    None => WriteKind::Other,
                },
                _ => WriteKind::Other,
            }
        };
        self.kinds.insert(ty, k);
        k
    }

    fn sorted_members(&mut self, ty: ecs_entity_t) -> R<Sorted> {
        if let Some(m) = self.sorted_cache.get(&ty) {
            return Ok(m.clone());
        }
        let mut ms: Vec<(Vec<u8>, Member)> = self
            .members(ty)?
            .iter()
            .map(|m| {
                let mut k = Vec::new();
                cbor::text(&mut k, &m.name);
                (k, m.clone())
            })
            .collect();
        ms.sort_by(|a, b| cbor::key_order(&a.1.name, &b.1.name));
        let ms: std::rc::Rc<[(Vec<u8>, Member)]> = ms.into();
        self.sorted_cache.insert(ty, ms.clone());
        Ok(ms)
    }

    /// Appends the DAG-CBOR of the value at `ptr` (a live value of `ty`): the same bytes as `encode(&read(ty, ptr))`.
    pub fn write_value(&mut self, ty: ecs_entity_t, ptr: *const u8, out: &mut Vec<u8>) -> R<()> {
        match self.write_kind(ty) {
            WriteKind::Primitive(k) => self.write_primitive(k, ptr, out),
            WriteKind::Struct => {
                let ms = self.sorted_members(ty)?;
                cbor::map(out, ms.len());
                for (key, m) in ms.iter() {
                    out.extend_from_slice(key);
                    // SAFETY: member offsets are within the struct.
                    let p = unsafe { ptr.add(m.offset) };
                    if m.count > 1 {
                        let stride = self.size(m.type_)?;
                        cbor::list(out, m.count as usize);
                        for i in 0..m.count as usize {
                            // SAFETY: an inline array of `count` elements.
                            self.write_value(m.type_, unsafe { p.add(i * stride) }, out)
                                .map_err(|e| member_err(&format!("{}[{i}]", m.name), e))?;
                        }
                    } else {
                        self.write_value(m.type_, p, out)
                            .map_err(|e| member_err(&m.name, e))?;
                    }
                }
                Ok(())
            }
            WriteKind::Array(et, n) => {
                let stride = self.size(et)?;
                cbor::list(out, n);
                for i in 0..n {
                    // SAFETY: an array of `n` elements.
                    self.write_value(et, unsafe { ptr.add(i * stride) }, out)
                        .map_err(|e| member_err(&format!("[{i}]"), e))?;
                }
                Ok(())
            }
            WriteKind::Vector(et) => {
                let stride = self.size(et)?;
                // SAFETY: a vector value is an ecs_vec_t holding `count` elements.
                let v = unsafe { &*ptr.cast::<sys::ecs_vec_t>() };
                let base = v.array.cast::<u8>();
                cbor::list(out, v.count as usize);
                for i in 0..v.count as usize {
                    // SAFETY: as above.
                    self.write_value(et, unsafe { base.add(i * stride) }, out)
                        .map_err(|e| member_err(&format!("[{i}]"), e))?;
                }
                Ok(())
            }
            WriteKind::Bytes => {
                // SAFETY: an ecs_vec_t of `count` bytes.
                let data = unsafe {
                    let v = &*ptr.cast::<sys::ecs_vec_t>();
                    if v.count == 0 {
                        &[][..]
                    } else {
                        std::slice::from_raw_parts(v.array.cast::<u8>(), v.count as usize)
                    }
                };
                cbor::bytes(out, data);
                Ok(())
            }
            WriteKind::Other => {
                let v = self.read_checked(ty, ptr)?;
                cbor::ipld(out, &v);
                Ok(())
            }
        }
    }

    /// Whether `ty`'s layout has a logical node (its values are checked against rules).
    fn refined(&mut self, ty: ecs_entity_t) -> bool {
        self.layout(ty).is_ok_and(|l| crate::logical::refined(&l))
    }

    /// The value at `ptr`, refused when its type's rules refuse it (SPEC.md, "Logical types": a value that breaks
    /// its rules is never saved, so a restore never meets one written by this library).
    fn read_checked(&mut self, ty: ecs_entity_t, ptr: *const u8) -> R<Ipld> {
        let v = self.read(ty, ptr)?;
        if self.refined(ty) {
            let l = self.layout(ty)?;
            crate::check::value(&v, &l, crate::check::Blobs::Links)?;
        }
        Ok(v)
    }

    /// Reads the DAG-CBOR value at `d`, saved with the layout the live type `ty` has now, over the live value at `dst`;
    /// with `dst` null it only checks the value (as `check::value` does) and resolves no ref. The direct path of a
    /// restore: no `Ipld` tree except for the kinds it hands to `world::write_at` (enums, bitmasks, vectors, bytes,
    /// opaque and described types, ids).
    pub fn read_into(
        &mut self,
        ty: ecs_entity_t,
        d: &mut cbor::Decoder<'_>,
        dst: *mut u8,
        ids: &mut dyn crate::described::Resolve,
    ) -> R<()> {
        let at = |p: *mut u8, off: usize| if p.is_null() { p } else { p.wrapping_add(off) };
        match self.write_kind(ty) {
            WriteKind::Primitive(k) => self.read_primitive(k, ty, d, dst, ids),
            WriteKind::Struct => {
                let ms = self.sorted_members(ty)?;
                let n = d.expect(5, "a struct value (a map)")?;
                if n != ms.len() as u64 {
                    return Err(format!(
                        "the struct value has {n} members, its type {}",
                        ms.len()
                    ));
                }
                for (_, m) in ms.iter() {
                    d.key(&m.name)
                        .map_err(|e| format!("the struct value's members: {e}"))?;
                    let p = at(dst, m.offset);
                    if m.count > 1 {
                        let stride = self.size(m.type_)?;
                        d.list_of(m.count as usize, "an inline array")
                            .map_err(|e| member_err(&m.name, e))?;
                        for i in 0..m.count as usize {
                            self.read_into(m.type_, d, at(p, i * stride), ids)
                                .map_err(|e| member_err(&format!("{}[{i}]", m.name), e))?;
                        }
                    } else {
                        self.read_into(m.type_, d, p, ids)
                            .map_err(|e| member_err(&m.name, e))?;
                    }
                }
                Ok(())
            }
            WriteKind::Array(et, n) => {
                let stride = self.size(et)?;
                d.list_of(n, "an array")?;
                for i in 0..n {
                    self.read_into(et, d, at(dst, i * stride), ids)
                        .map_err(|e| member_err(&format!("[{i}]"), e))?;
                }
                Ok(())
            }
            WriteKind::Vector(_) | WriteKind::Bytes | WriteKind::Other => {
                self.read_other(ty, d, dst, ids)
            }
        }
    }

    fn read_other(
        &mut self,
        ty: ecs_entity_t,
        d: &mut cbor::Decoder<'_>,
        dst: *mut u8,
        ids: &mut dyn crate::described::Resolve,
    ) -> R<()> {
        let v = d.value(0)?;
        let l = self.layout(ty)?;
        crate::check::value(&v, &l, crate::check::Blobs::Links)?;
        crate::logical::run_checks(self.w, &v, &l)?;
        if dst.is_null() {
            return Ok(());
        }
        crate::world::write_at(self.w.cast_mut(), self.s, ty, dst, &v, &l, ids)
    }

    fn read_primitive(
        &mut self,
        kind: i32,
        ty: ecs_entity_t,
        d: &mut cbor::Decoder<'_>,
        dst: *mut u8,
        ids: &mut dyn crate::described::Resolve,
    ) -> R<()> {
        let w = !dst.is_null();
        macro_rules! int {
            ($t:ty) => {{
                let i = d.int()?;
                let x = <$t>::try_from(i)
                    .map_err(|_| format!("{i} does not fit {}", stringify!($t)))?;
                if w {
                    // SAFETY: `dst` is a live value of this primitive kind.
                    unsafe { dst.cast::<$t>().write_unaligned(x) };
                }
            }};
        }
        match kind {
            sys::EcsBool => {
                let b = match d.peek()? {
                    0xf4 => false,
                    0xf5 => true,
                    _ => return Err("not a bool value".into()),
                };
                d.value(0)?;
                if w {
                    // SAFETY: as above.
                    unsafe { dst.write(b as u8) };
                }
            }
            sys::EcsChar | sys::EcsI8 => int!(i8),
            sys::EcsByte | sys::EcsU8 => int!(u8),
            sys::EcsU16 => int!(u16),
            sys::EcsU32 => int!(u32),
            sys::EcsU64 => int!(u64),
            sys::EcsI16 => int!(i16),
            sys::EcsI32 => int!(i32),
            sys::EcsI64 => int!(i64),
            sys::EcsUPtr => int!(usize),
            sys::EcsIPtr => int!(isize),
            sys::EcsF32 => {
                let f = d.float()?;
                if w {
                    // SAFETY: as above.
                    unsafe { dst.cast::<f32>().write_unaligned(f as f32) };
                }
            }
            sys::EcsF64 => {
                let f = d.float()?;
                if w {
                    // SAFETY: as above.
                    unsafe { dst.cast::<f64>().write_unaligned(f) };
                }
            }
            sys::EcsString => {
                let s = if d.peek()? == 0xf6 {
                    d.value(0)?;
                    None
                } else {
                    let s = d.str()?;
                    if s.contains('\0') {
                        return Err("a string contains a NUL byte".into());
                    }
                    Some(s)
                };
                if w {
                    let (p, n) = s.map_or((core::ptr::null(), 0), |s| {
                        (s.as_ptr().cast::<c_char>(), s.len())
                    });
                    let n = i32::try_from(n).map_err(|_| "a string too long for Flecs")?;
                    // SAFETY: `dst` is a live Flecs string slot; `p` holds `n` bytes.
                    if unsafe { sys::glue_set_string(dst.cast(), p, n) } != 0 {
                        return Err("out of memory for a string".into());
                    }
                }
            }
            sys::EcsEntity => {
                let v = d.value(0)?;
                let e = match v {
                    Ipld::Null => 0,
                    r => {
                        crate::check::reference(&r, "entity value")?;
                        if w { ids.entity(&r)? } else { 0 }
                    }
                };
                if w {
                    // SAFETY: as above.
                    unsafe { dst.cast::<u64>().write_unaligned(e) };
                }
            }
            _ => return self.read_other(ty, d, dst, ids),
        }
        Ok(())
    }

    fn write_primitive(&mut self, kind: i32, p: *const u8, out: &mut Vec<u8>) -> R<()> {
        // SAFETY: `p` points at a value of the primitive kind.
        unsafe {
            match kind {
                sys::EcsBool => cbor::bool(out, p.read() != 0),
                sys::EcsChar | sys::EcsI8 => cbor::int(out, p.cast::<i8>().read() as i128),
                sys::EcsByte | sys::EcsU8 => cbor::int(out, p.read() as i128),
                sys::EcsU16 => cbor::int(out, p.cast::<u16>().read_unaligned() as i128),
                sys::EcsU32 => cbor::int(out, p.cast::<u32>().read_unaligned() as i128),
                sys::EcsU64 => cbor::int(out, p.cast::<u64>().read_unaligned() as i128),
                sys::EcsI16 => cbor::int(out, p.cast::<i16>().read_unaligned() as i128),
                sys::EcsI32 => cbor::int(out, p.cast::<i32>().read_unaligned() as i128),
                sys::EcsI64 => cbor::int(out, p.cast::<i64>().read_unaligned() as i128),
                sys::EcsUPtr => cbor::int(out, p.cast::<usize>().read_unaligned() as i128),
                sys::EcsIPtr => cbor::int(out, p.cast::<isize>().read_unaligned() as i128),
                sys::EcsF32 | sys::EcsF64 => {
                    let v = if kind == sys::EcsF32 {
                        p.cast::<f32>().read_unaligned() as f64
                    } else {
                        p.cast::<f64>().read_unaligned()
                    };
                    float(v)?;
                    cbor::float(out, v);
                }
                _ => {
                    let v = self.primitive(kind, p)?;
                    cbor::ipld(out, &v);
                }
            }
        }
        Ok(())
    }

    /// The value type `ty`'s constructor makes (zeroed memory without one), in its snapshot shape.
    pub fn default_value(&mut self, ty: ecs_entity_t) -> R<Ipld> {
        // SAFETY: a new writer over a default-constructed value of `ty`, freed below.
        let wr = unsafe { sys::glue_writer_new(self.w, ty) };
        if wr.is_null() {
            return Err("cannot make a value of it".into());
        }
        // SAFETY: the writer's value is live until it is freed.
        let r = self.read(ty, unsafe { sys::glue_writer_ptr(wr) }.cast());
        // SAFETY: freed once.
        unsafe { sys::glue_writer_free(self.w, wr) };
        r
    }

    fn size(&self, ty: ecs_entity_t) -> R<usize> {
        self.get::<sys::EcsComponent>(ty, self.b.component)
            .map(|c| c.size as usize)
            .ok_or_else(|| "type has no size".into())
    }

    fn type_name(&mut self, ty: ecs_entity_t) -> Ipld {
        self.path(ty).map_or(Ipld::Null, Ipld::String)
    }

    fn target_ref(&mut self, e: ecs_entity_t, rel: ecs_entity_t) -> R<Ipld> {
        match self.target(e, rel) {
            0 => Ok(Ipld::Null),
            t => self.reference(t),
        }
    }

    /// The merge rule declared on an entity with `(Merge, Rule)`.
    fn rule_of(&mut self, e: ecs_entity_t) -> R<Option<Rule>> {
        let t = self.target(e, self.s.merge);
        if t == 0 {
            return Ok(None);
        }
        match self.s.rule(t) {
            Some(r) => Ok(Some(r)),
            None => Err(format!("(Merge, {}) is not a merge rule", self.path(t)?)),
        }
    }

    /// The layout of a component or pair type (SPEC.md, "Layouts").
    pub fn layout(&mut self, ty: ecs_entity_t) -> R<Ipld> {
        if let Some(l) = self.layout_cache.get(&ty) {
            return Ok(l.clone());
        }
        let l = self.layout_in(ty, false)?;
        self.layout_cache.insert(ty, l.clone());
        Ok(l)
    }

    /// Whether a value of `ty` is all its bytes: no pointer to memory the value owns (no string, vector, opaque or
    /// described type), so two equal byte strings are equal values. Entity members count, with the refs caveat of
    /// SPEC.md, "Incremental hashing".
    pub fn is_pod(&mut self, ty: ecs_entity_t) -> bool {
        fn pod(l: &Ipld) -> bool {
            match l {
                Ipld::String(p) => p != "string" && p != "bytes",
                Ipld::Map(m) => {
                    if let Some(Ipld::List(ms)) = m.get("members") {
                        ms.iter()
                            .all(|x| matches!(x, Ipld::List(x) if x.get(1).is_some_and(pod)))
                    } else if let Some(e) = m.get("array") {
                        pod(e)
                    } else {
                        m.contains_key("enum") || m.contains_key("bitmask")
                    }
                }
                _ => false,
            }
        }
        if self.described(ty).is_some() {
            return false;
        }
        self.layout(ty).is_ok_and(|l| pod(&l))
    }

    /// A type's path for messages ("" when it has none), cached.
    fn tp(&mut self, ty: ecs_entity_t) -> std::rc::Rc<str> {
        if let Some(n) = self.names_cache.get(&ty) {
            return n.clone();
        }
        let n: std::rc::Rc<str> = self.path(ty).unwrap_or_default().into();
        self.names_cache.insert(ty, n.clone());
        n
    }

    /// The description of a type, when the host gave one (SPEC.md, "Described types").
    fn described(&self, ty: ecs_entity_t) -> Option<&'static crate::described::Described> {
        crate::described::of(self.w, self.s.described, ty)
    }

    /// Whether a type is a struct (merged member by member unless a rule covers it).
    fn is_struct(&self, ty: ecs_entity_t) -> bool {
        if let Some(d) = self.described(ty) {
            return matches!(&d.layout, Ipld::Map(m) if m.contains_key("struct"));
        }
        ty != self.s.bytes
            && ty != self.s.blob
            && self.get::<sys::EcsType>(ty, self.b.type_).map(|t| t.kind)
                == Some(sys::EcsStructType)
    }

    /// The layout of a type. `covered` is set inside a value that one rule merges whole (a member or component with a
    /// rule, or an element of an array or vector): there, struct members carry no rule (null), and rules declared
    /// inside are not used (SPEC.md, "Merge rules").
    fn layout_in(&mut self, ty: ecs_entity_t, covered: bool) -> R<Ipld> {
        let l = self.physical_layout(ty, covered)?;
        if self.described(ty).is_some() || ty == self.s.bytes || ty == self.s.blob {
            return Ok(l);
        }
        // A type's own rules (a struct with `(Validated, check)`, say) refine its whole value.
        let (w, vocab) = (self.w, self.s.vocab);
        let rules = crate::logical::own_rules(w, &vocab, ty, &mut |t| self.path(t))?;
        if rules.is_empty() {
            return Ok(l);
        }
        crate::logical::node(None, l, rules).map_err(|e| format!("type {}: {e}", self.tp(ty)))
    }

    /// The rules entity `e` (a struct member, or a logical type it is an instance of) declares, with the rules of
    /// every logical type it is `(IsA, ...)` of, applied to `l`, its type's layout. A logical type's physical type
    /// (its `(IsA, flecs.meta.<primitive>)`) must be `l`'s.
    fn refine(&mut self, e: ecs_entity_t, l: Ipld) -> R<Ipld> {
        let (name, rules, phys) = self.entity_rules(e, 0)?;
        if let Some(p) = phys
            && crate::logical::physical(&l) != &Ipld::String(p.clone())
        {
            let n = name.as_deref().unwrap_or("its logical type");
            return Err(format!("{n} refines {p}, but the member's type is not {p}"));
        }
        if name.is_none() && rules.is_empty() {
            return Ok(l);
        }
        crate::logical::node(name, l, rules)
    }

    /// (nearest logical type's path, merged rules, physical primitive) of `e`.
    #[allow(clippy::type_complexity)]
    fn entity_rules(
        &mut self,
        e: ecs_entity_t,
        depth: usize,
    ) -> R<(Option<String>, BTreeMap<String, Ipld>, Option<String>)> {
        if depth > 32 {
            return Err("logical types nest more than 32 deep (an IsA cycle?)".into());
        }
        let (w, vocab) = (self.w, self.s.vocab);
        let mut rules = crate::logical::own_rules(w, &vocab, e, &mut |t| self.path(t))?;
        let (mut name, mut phys) = (None, None);
        let mut bases = Vec::new();
        for id in self.ids(e) {
            if sys::is_pair(id) && self.alive(sys::pair_first(id)) == self.b.is_a {
                bases.push(self.alive(sys::pair_second(id)));
            }
        }
        for base in bases {
            if let Some(p) = self.get::<sys::EcsPrimitive>(base, self.b.primitive) {
                let p = primitive_name(p.kind)
                    .ok_or_else(|| format!("unknown primitive kind {}", p.kind))?;
                phys = Some(p.to_string());
                continue;
            }
            let (_, br, bp) = self.entity_rules(base, depth + 1)?;
            let bn = self.path(base)?;
            rules = crate::logical::merge_rules(&br, &rules).map_err(|e| format!("{bn}: {e}"))?;
            name = name.or(Some(bn));
            if let (Some(a), Some(b)) = (&phys, &bp)
                && a != b
            {
                return Err(format!("its logical types refine {a} and {b}"));
            }
            phys = phys.or(bp);
        }
        Ok((name, rules, phys))
    }

    /// A type's layout before its own rules.
    fn physical_layout(&mut self, ty: ecs_entity_t, covered: bool) -> R<Ipld> {
        if ty == self.s.bytes {
            return Ok(text("bytes"));
        }
        if ty == self.s.blob {
            return Ok(text("blob"));
        }
        let tp = self
            .path(ty)
            .unwrap_or_else(|_| format!("#{}", ty & 0xffff_ffff));
        if let Some(d) = self.described(ty) {
            let whole = if covered { None } else { self.rule_of(ty)? };
            return crate::check::fill_rules(&d.layout, covered, whole)
                .map_err(|e| format!("type {tp}: {e}"));
        }
        let kind = self
            .get::<sys::EcsType>(ty, self.b.type_)
            .map(|t| t.kind)
            .ok_or_else(|| format!("type {tp} has no reflection data"))?;
        Ok(match kind {
            sys::EcsPrimitiveType => {
                let k = self
                    .get::<sys::EcsPrimitive>(ty, self.b.primitive)
                    .ok_or("no primitive kind")?
                    .kind;
                text(
                    primitive_name(k)
                        .ok_or_else(|| format!("type {tp}: unknown primitive kind {k}"))?,
                )
            }
            sys::EcsEnumType | sys::EcsBitmaskType => {
                let bitmask = kind == sys::EcsBitmaskType;
                let list = self
                    .constant_values(ty, bitmask)?
                    .into_iter()
                    .map(|(n, v)| Ipld::List(vec![text(n), Ipld::Integer(v)]))
                    .collect();
                let p = self.type_name(ty);
                if bitmask {
                    map([("bitmask", p), ("constants", Ipld::List(list))])
                } else {
                    let under = self
                        .get::<sys::EcsEnum>(ty, self.b.enum_)
                        .ok_or("no enum data")?
                        .underlying_type;
                    map([
                        ("enum", p),
                        ("underlying", self.layout_in(under, true)?),
                        ("constants", Ipld::List(list)),
                    ])
                }
            }
            sys::EcsStructType => {
                let whole = if covered { None } else { self.rule_of(ty)? };
                let mut members = Vec::new();
                for m in self.members(ty)?.iter() {
                    let own = if m.entity != 0 && !covered {
                        self.rule_of(m.entity)?
                    } else {
                        None
                    };
                    // A member's rule overrides the struct's; a nested struct without either merges field by field;
                    // anything else gets the default. Inside a covered value no member has a rule of its own.
                    let nested = m.count <= 1 && self.is_struct(m.type_);
                    let rule = if covered {
                        None
                    } else {
                        match own.or(whole) {
                            Some(r) => Some(r),
                            None if nested => None,
                            None => Some(Rule::DEFAULT),
                        }
                    };
                    let inner = covered || rule.is_some();
                    let mut l = self
                        .layout_in(m.type_, inner)
                        .map_err(|e| member_err(&m.name, e))?;
                    if m.entity != 0 {
                        // The member's rules, its own and its logical type's (SPEC.md, "Logical types"); on an inline
                        // array they refine each element.
                        l = self
                            .refine(m.entity, l)
                            .map_err(|e| member_err(&m.name, e))?;
                    }
                    if m.count > 1 {
                        l = map([("array", l), ("count", Ipld::Integer(m.count as i128))]);
                    }
                    if let Some(r) = rule {
                        r.check(&l).map_err(|e| member_err(&m.name, e))?;
                    }
                    members.push(Ipld::List(vec![
                        text(m.name.clone()),
                        l,
                        rule.map_or(Ipld::Null, |r| text(r.name())),
                    ]));
                }
                map([
                    ("struct", self.type_name(ty)),
                    ("members", Ipld::List(members)),
                ])
            }
            sys::EcsArrayType => {
                let a = self
                    .get::<sys::EcsArray>(ty, self.b.array)
                    .ok_or("no array data")?;
                let (et, n) = (a.type_, a.count);
                map([
                    ("array", self.layout_in(et, true)?),
                    ("count", Ipld::Integer(n as i128)),
                ])
            }
            sys::EcsVectorType => {
                let et = self
                    .get::<sys::EcsVector>(ty, self.b.vector)
                    .ok_or("no vector data")?
                    .type_;
                map([("vector", self.layout_in(et, true)?)])
            }
            sys::EcsOpaqueType => {
                // SAFETY: plain query.
                let as_type = unsafe { sys::glue_opaque_as_type(self.w, ty) };
                if as_type == 0 {
                    return Err(format!("type {tp} is opaque with no as_type"));
                }
                return self.layout_in(as_type, covered);
            }
            sys::EcsMapType => return Err(format!("type {tp} is a map: not supported")),
            _ => return Err(format!("type {tp} has an unsupported kind ({kind})")),
        })
    }

    /// The layout record of a component or pair type, made once per type: `{"layout", "merge", "sync", "writable_by",
    /// "validated_by", "hash"}`, where `merge` is the whole value's rule for a non-struct type (null for structs, whose
    /// members carry theirs) and `hash` is the first 8 bytes of SHA-256 over the record without `hash`.
    fn layout_record(&mut self, ty: ecs_entity_t, ephemeral: bool) -> R<Ipld> {
        self.used.insert(ty);
        let tp = self.path(ty)?;
        let cache = if ephemeral {
            &self.ephemeral_layouts
        } else {
            &self.layouts
        };
        if let Some(Ipld::Map(rec)) = cache.get(&tp) {
            return Ok(rec["hash"].clone());
        }
        let l = self.layout(ty)?;
        let is_struct = crate::check::is_struct(&l);
        let merge = if is_struct {
            Ipld::Null
        } else {
            let r = self.rule_of(ty)?.unwrap_or(Rule::DEFAULT);
            r.check(&l).map_err(|e| format!("type {tp}: {e}"))?;
            text(r.name())
        };
        // Reserved, not enforced (SPEC.md, "Permissions", "Validation"): who may write the type and what validates it.
        let (wb, vb) = (self.s.writable_by, self.s.validated_by);
        let writable_by = self.target_ref(ty, wb)?;
        let validated_by = self.target_ref(ty, vb)?;
        let (base, version) = crate::check::base_version(&tp)?;
        let mut body = BTreeMap::new();
        body.insert("base".to_string(), text(base));
        body.insert("version".to_string(), Ipld::Integer(version as i128));
        body.insert("layout".to_string(), l);
        body.insert("merge".to_string(), merge);
        body.insert("writable_by".to_string(), writable_by);
        body.insert("validated_by".to_string(), validated_by);
        body.insert("sync".to_string(), text(self.mode_of(ty)?.name()));
        let hash = Ipld::Bytes(hash8(&encode(&Ipld::Map(body.clone()))));
        body.insert("hash".to_string(), hash.clone());
        if ephemeral {
            self.ephemeral_layouts.insert(tp, Ipld::Map(body));
        } else {
            self.layouts.insert(tp, Ipld::Map(body));
        }
        Ok(hash)
    }

    /// (name, value) in registration order. A bitmask constant's value is its first slot (ecs_bitmask_constant_t); an
    /// enum constant's is `value_unsigned` when the underlying type is unsigned and `value` otherwise, as Flecs keeps
    /// only the one that matches.
    fn constant_values(&self, ty: ecs_entity_t, bitmask: bool) -> R<Vec<(String, i128)>> {
        let unsigned = if bitmask {
            true
        } else {
            let under = self
                .get::<sys::EcsEnum>(ty, self.b.enum_)
                .ok_or("no enum data")?
                .underlying_type;
            let k = self
                .get::<sys::EcsPrimitive>(under, self.b.primitive)
                .ok_or("enum without a primitive underlying type")?
                .kind;
            matches!(
                k,
                sys::EcsByte | sys::EcsU8 | sys::EcsU16 | sys::EcsU32 | sys::EcsU64 | sys::EcsUPtr
            )
        };
        Ok(self
            .constants(ty)?
            .into_iter()
            .map(|(n, v, u)| {
                let x = if bitmask {
                    v as u64 as i128
                } else if unsigned {
                    u as i128
                } else {
                    v as i128
                };
                (n, x)
            })
            .collect())
    }

    /// (name, signed value, unsigned value) in registration order.
    fn constants(&self, ty: ecs_entity_t) -> R<Vec<(String, i64, u64)>> {
        let c = self
            .get::<sys::EcsConstants>(ty, self.b.constants)
            .ok_or("no constants")?;
        let v = &c.ordered_constants;
        let mut out = Vec::new();
        for i in 0..v.count as usize {
            // SAFETY: the vector holds `count` ecs_enum_constant_t (bitmask constants share the layout).
            let k = unsafe { &*v.array.cast::<sys::ecs_enum_constant_t>().add(i) };
            out.push((cstr(k.name).unwrap_or_default(), k.value, k.value_unsigned));
        }
        Ok(out)
    }

    fn members(&mut self, ty: ecs_entity_t) -> R<std::rc::Rc<[Member]>> {
        if let Some(m) = self.members_cache.get(&ty) {
            return Ok(m.clone());
        }
        let s = self
            .get::<sys::EcsStruct>(ty, self.b.struct_)
            .ok_or("no struct data")?;
        let v = &s.members;
        let mut out = Vec::new();
        for i in 0..v.count as usize {
            // SAFETY: the vector holds `count` ecs_member_t.
            let m = unsafe { &*v.array.cast::<sys::ecs_member_t>().add(i) };
            out.push(Member {
                name: cstr(m.name).unwrap_or_default(),
                type_: m.type_,
                count: m.count,
                offset: m.offset as usize,
                entity: m.member,
            });
        }
        let out: std::rc::Rc<[Member]> = out.into();
        self.members_cache.insert(ty, out.clone());
        Ok(out)
    }

    /// (layout hash, value) of a component or pair value.
    fn value(&mut self, ty: ecs_entity_t, ptr: *const u8, ephemeral: bool) -> R<(Ipld, Ipld)> {
        if ptr.is_null() {
            return Err("no value".into());
        }
        let hash = self.layout_record(ty, ephemeral)?;
        Ok((hash, self.read_checked(ty, ptr)?))
    }

    fn read(&mut self, ty: ecs_entity_t, ptr: *const u8) -> R<Ipld> {
        if ty == self.s.bytes || ty == self.s.blob {
            // SAFETY: both are vectors of u8: an ecs_vec_t of `count` bytes.
            let data = unsafe {
                let v = &*ptr.cast::<sys::ecs_vec_t>();
                if v.count == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(v.array.cast::<u8>(), v.count as usize)
                }
            };
            if ty == self.s.bytes {
                return Ok(Ipld::Bytes(data.to_vec()));
            }
            if self.in_ephemeral {
                return Err(
                    "a Blob cannot be Ephemeral (it needs stored blocks); use Bytes".into(),
                );
            }
            if self.host.is_some() {
                return Err("a Blob in a host world is not supported yet; use Bytes".into());
            }
            return self.blob(data).map(Ipld::Link);
        }
        if let Some(d) = self.described(ty) {
            // SAFETY: `ptr` is a live value of `ty`, which `d` describes.
            return unsafe { (d.read)(ptr, self) };
        }
        let tp = self.tp(ty);
        let kind = self
            .get::<sys::EcsType>(ty, self.b.type_)
            .map(|t| t.kind)
            .ok_or_else(|| format!("type {tp} has no reflection data"))?;
        match kind {
            sys::EcsPrimitiveType => {
                let k = self
                    .get::<sys::EcsPrimitive>(ty, self.b.primitive)
                    .ok_or("no primitive kind")?
                    .kind;
                self.primitive(k, ptr)
            }
            sys::EcsEnumType => {
                let under = self
                    .get::<sys::EcsEnum>(ty, self.b.enum_)
                    .ok_or("no enum data")?
                    .underlying_type;
                let k = self
                    .get::<sys::EcsPrimitive>(under, self.b.primitive)
                    .ok_or("enum without a primitive underlying type")?
                    .kind;
                let Ipld::Integer(raw) = self.primitive(k, ptr)? else {
                    return Err(format!("enum {tp}: underlying type is not an integer"));
                };
                let hit = self
                    .constant_values(ty, false)?
                    .into_iter()
                    .find(|(_, v)| *v == raw);
                match hit {
                    Some((name, _)) => Ok(text(name)),
                    None => Err(format!("value {raw} is not a constant of enum {tp}")),
                }
            }
            sys::EcsBitmaskType => {
                // SAFETY: bitmasks are stored as u32.
                let mut rest = unsafe { ptr.cast::<u32>().read_unaligned() } as u64;
                let mut names = Vec::new();
                // A bitmask constant's value is in the first value slot (ecs_bitmask_constant_t).
                for (name, v, _) in self.constants(ty)? {
                    let v = v as u64;
                    if v != 0 && rest & v == v {
                        names.push(text(name));
                        rest &= !v;
                    }
                }
                if rest != 0 {
                    return Err(format!("bits {rest:#x} are not constants of bitmask {tp}"));
                }
                Ok(Ipld::List(names))
            }
            sys::EcsStructType => {
                let mut entries = BTreeMap::new();
                for m in self.members(ty)?.iter() {
                    // SAFETY: member offsets are within the struct.
                    let p = unsafe { ptr.add(m.offset) };
                    let v = if m.count > 1 {
                        let stride = self.size(m.type_)?;
                        // SAFETY: an inline array of `count` elements.
                        (0..m.count as usize)
                            .map(|i| {
                                self.read(m.type_, unsafe { p.add(i * stride) })
                                    .map_err(|e| member_err(&format!("{}[{i}]", m.name), e))
                            })
                            .collect::<R<_>>()
                            .map(Ipld::List)?
                    } else {
                        self.read(m.type_, p).map_err(|e| member_err(&m.name, e))?
                    };
                    entries.insert(m.name.clone(), v);
                }
                Ok(Ipld::Map(entries))
            }
            sys::EcsArrayType => {
                let a = self
                    .get::<sys::EcsArray>(ty, self.b.array)
                    .ok_or("no array data")?;
                let (et, n) = (a.type_, a.count as usize);
                let stride = self.size(et)?;
                // SAFETY: an array of `n` elements.
                Ok(Ipld::List(
                    (0..n)
                        .map(|i| {
                            self.read(et, unsafe { ptr.add(i * stride) })
                                .map_err(|e| member_err(&format!("[{i}]"), e))
                        })
                        .collect::<R<_>>()?,
                ))
            }
            sys::EcsVectorType => {
                let et = self
                    .get::<sys::EcsVector>(ty, self.b.vector)
                    .ok_or("no vector data")?
                    .type_;
                let stride = self.size(et)?;
                // SAFETY: a vector value is an ecs_vec_t holding `count` elements.
                let v = unsafe { &*ptr.cast::<sys::ecs_vec_t>() };
                let base = v.array.cast::<u8>();
                // SAFETY: as above.
                Ok(Ipld::List(
                    (0..v.count as usize)
                        .map(|i| {
                            self.read(et, unsafe { base.add(i * stride) })
                                .map_err(|e| member_err(&format!("[{i}]"), e))
                        })
                        .collect::<R<_>>()?,
                ))
            }
            sys::EcsOpaqueType => self.read_opaque(ty, ptr, &tp),
            _ => self.layout(ty).map(|_| Ipld::Null), // layout() names the unsupported kind
        }
    }

    /// An opaque value, read through its type's serialize callback, in the shape of its `as_type`.
    fn read_opaque(&mut self, ty: ecs_entity_t, ptr: *const u8, tp: &str) -> R<Ipld> {
        // SAFETY: plain query.
        let as_type = unsafe { sys::glue_opaque_as_type(self.w, ty) };
        let kind = self
            .get::<sys::EcsType>(as_type, self.b.type_)
            .map(|t| t.kind)
            .ok_or_else(|| format!("opaque {tp}: its as_type has no reflection data"))?;
        let mut cx = Opaque {
            walk: self,
            items: Vec::new(),
            member: None,
            err: None,
        };
        // SAFETY: `ptr` is a live value of `ty`; the callbacks get `cx` back as their context and only run during
        // this call.
        let rc = unsafe {
            sys::glue_opaque_serialize(
                cx.walk.w,
                ty,
                ptr.cast(),
                opaque_value,
                opaque_member,
                (&raw mut cx).cast(),
            )
        };
        if let Some(e) = cx.err {
            return Err(e);
        }
        if rc != 0 {
            return Err(format!("opaque {tp}: its serialize callback failed ({rc})"));
        }
        let items = cx.items;
        match kind {
            sys::EcsVectorType | sys::EcsArrayType => {
                Ok(Ipld::List(items.into_iter().map(|(_, v)| v).collect()))
            }
            sys::EcsStructType => Ok(Ipld::Map(
                items
                    .into_iter()
                    .map(|(n, v)| (n.unwrap_or_default(), v))
                    .collect(),
            )),
            _ => {
                let n = items.len();
                let mut it = items.into_iter();
                match (it.next(), n) {
                    (Some((_, v)), 1) => Ok(v),
                    _ => Err(format!(
                        "opaque {tp}: its serialize callback gave {n} values for a single value"
                    )),
                }
            }
        }
    }

    fn keep_blob(&mut self, cid: Cid, bytes: Vec<u8>) {
        if self.blob_seen.insert(cid) {
            self.blobs.push((cid, bytes));
        }
    }

    /// A blob as blocks (SPEC.md, "Blobs"): one raw block when it is a single chunk, otherwise raw chunks under a
    /// DAG-CBOR chunk list `[[length, link], ...]`. Returns the link the field holds.
    fn blob(&mut self, data: &[u8]) -> R<Cid> {
        let chunks: Vec<_> =
            fastcdc::v2020::FastCDC::new(data, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX).collect();
        if chunks.len() <= 1 {
            let cid = dag::cid_with(RAW, data);
            self.keep_blob(cid, data.to_vec());
            return Ok(cid);
        }
        if chunks.len() > MAX_CHUNKS {
            return Err(format!(
                "a blob of {} bytes has {} chunks; more than {MAX_CHUNKS} is not supported yet",
                data.len(),
                chunks.len()
            ));
        }
        let mut list = Vec::with_capacity(chunks.len());
        for c in chunks {
            let bytes = &data[c.offset..c.offset + c.length];
            let cid = dag::cid_with(RAW, bytes);
            self.keep_blob(cid, bytes.to_vec());
            list.push(Ipld::List(vec![
                Ipld::Integer(c.length as i128),
                Ipld::Link(cid),
            ]));
        }
        let index = encode(&Ipld::List(list));
        let cid = dag::cid_with(DAG_CBOR, &index);
        self.keep_blob(cid, index);
        Ok(cid)
    }

    fn primitive(&mut self, kind: i32, p: *const u8) -> R<Ipld> {
        // SAFETY: `p` points at a value of the primitive kind.
        unsafe {
            Ok(match kind {
                sys::EcsBool => Ipld::Bool(p.read() != 0),
                sys::EcsChar | sys::EcsI8 => Ipld::Integer(p.cast::<i8>().read() as i128),
                sys::EcsByte | sys::EcsU8 => Ipld::Integer(p.read() as i128),
                sys::EcsU16 => Ipld::Integer(p.cast::<u16>().read_unaligned() as i128),
                sys::EcsU32 => Ipld::Integer(p.cast::<u32>().read_unaligned() as i128),
                sys::EcsU64 => Ipld::Integer(p.cast::<u64>().read_unaligned() as i128),
                sys::EcsI16 => Ipld::Integer(p.cast::<i16>().read_unaligned() as i128),
                sys::EcsI32 => Ipld::Integer(p.cast::<i32>().read_unaligned() as i128),
                sys::EcsI64 => Ipld::Integer(p.cast::<i64>().read_unaligned() as i128),
                sys::EcsF32 => float(p.cast::<f32>().read_unaligned() as f64)?,
                sys::EcsF64 => float(p.cast::<f64>().read_unaligned())?,
                sys::EcsUPtr => Ipld::Integer(p.cast::<usize>().read_unaligned() as i128),
                sys::EcsIPtr => Ipld::Integer(p.cast::<isize>().read_unaligned() as i128),
                sys::EcsString => {
                    let s = p.cast::<*const c_char>().read_unaligned();
                    if s.is_null() {
                        Ipld::Null
                    } else {
                        text(
                            CStr::from_ptr(s)
                                .to_str()
                                .map_err(|_| "string is not valid UTF-8")?,
                        )
                    }
                }
                sys::EcsEntity => {
                    let e = p.cast::<u64>().read_unaligned();
                    if e == 0 {
                        Ipld::Null
                    } else {
                        self.reference(e)?
                    }
                }
                sys::EcsId => {
                    let id = p.cast::<u64>().read_unaligned();
                    if id == 0 {
                        Ipld::Null
                    } else if sys::is_pair(id) {
                        let (r, t) = (
                            self.alive(sys::pair_first(id)),
                            self.alive(sys::pair_second(id)),
                        );
                        Ipld::List(vec![self.reference(r)?, self.reference(t)?])
                    } else if id & sys::ECS_ID_FLAGS_MASK != 0 {
                        return Err(format!("id {id:#x} has flags: not supported"));
                    } else {
                        self.reference(id)?
                    }
                }
                k => return Err(format!("unknown primitive kind {k}")),
            })
        }
    }
}

impl crate::described::Refs for Walk<'_> {
    fn reference(&mut self, e: ecs_entity_t) -> R<Ipld> {
        if e == 0 {
            return Ok(Ipld::Null);
        }
        Walk::reference(self, e)
    }
}

/// The state `glue_opaque_serialize`'s callbacks fill.
struct Opaque<'a, 's> {
    walk: &'a mut Walk<'s>,
    items: Vec<(Option<String>, Ipld)>,
    member: Option<String>,
    err: Option<String>,
}

unsafe extern "C" fn opaque_value(
    ctx: *mut std::ffi::c_void,
    ty: ecs_entity_t,
    ptr: *const std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: `ctx` is the `Opaque` read_opaque passed, alive for the call.
    let cx = unsafe { &mut *ctx.cast::<Opaque<'_, '_>>() };
    let at = cx.items.len();
    match cx.walk.read(ty, ptr.cast()) {
        Ok(v) => {
            let name = cx.member.take();
            cx.items.push((name, v));
            0
        }
        Err(e) => {
            if cx.err.is_none() {
                cx.err = Some(match &cx.member {
                    Some(n) => member_err(n, e),
                    None => member_err(&format!("[{at}]"), e),
                });
            }
            -1
        }
    }
}

unsafe extern "C" fn opaque_member(
    ctx: *mut std::ffi::c_void,
    name: *const c_char,
) -> std::ffi::c_int {
    // SAFETY: as in opaque_value; `name` is a NUL-terminated member name.
    let cx = unsafe { &mut *ctx.cast::<Opaque<'_, '_>>() };
    cx.member = cstr(name);
    0
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// DAG-CBOR has no NaN or infinities: refuse them rather than write something else.
pub(crate) fn float(v: f64) -> R<Ipld> {
    if v.is_finite() {
        Ok(Ipld::Float(v))
    } else {
        Err(format!(
            "{v} cannot be stored: DAG-CBOR allows only finite floats"
        ))
    }
}

#[derive(Clone)]
struct Member {
    name: String,
    type_: ecs_entity_t,
    count: i32,
    offset: usize,
    entity: ecs_entity_t,
}

pub fn primitive_name(kind: i32) -> Option<&'static str> {
    Some(match kind {
        sys::EcsBool => "bool",
        sys::EcsChar => "char",
        sys::EcsByte => "byte",
        sys::EcsU8 => "u8",
        sys::EcsU16 => "u16",
        sys::EcsU32 => "u32",
        sys::EcsU64 => "u64",
        sys::EcsI8 => "i8",
        sys::EcsI16 => "i16",
        sys::EcsI32 => "i32",
        sys::EcsI64 => "i64",
        sys::EcsF32 => "f32",
        sys::EcsF64 => "f64",
        sys::EcsUPtr => "uptr",
        sys::EcsIPtr => "iptr",
        sys::EcsString => "string",
        sys::EcsEntity => "entity",
        sys::EcsId => "id",
        _ => return None,
    })
}
