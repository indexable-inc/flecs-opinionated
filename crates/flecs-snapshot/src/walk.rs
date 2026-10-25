//! Walks a world into snapshot records (SPEC.md, "Snapshot"): the entities under the roots, depth first, each with
//! its stable id, tags, pairs, component values and prefabs, plus the layout record of every type a value uses.
//! Entities are referred to by stable id, or by path when they have none (Flecs's builtins); never by Flecs id.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{CStr, CString, c_char};

use flecs_c as sys;
use ipld_core::ipld::Ipld;
use sha2::{Digest, Sha256};
use sys::{ecs_entity_t, ecs_id_t, ecs_world_t};

use crate::merge::Rule;
use crate::schema::Schema;

/// Children of one parent past this count are refused for now (SPEC.md, "Large child sets").
pub const MAX_CHILDREN: usize = 4096;

/// The ids of Flecs's builtins (C globals, set once the first world exists).
struct Builtins {
    child_of: ecs_entity_t,
    is_a: ecs_entity_t,
    prefab: ecs_entity_t,
    ordered_children: ecs_entity_t,
    on_instantiate: ecs_entity_t,
    dont_inherit: ecs_entity_t,
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
                dont_inherit: sys::EcsDontInherit,
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
}

pub struct Snapshot {
    pub items: Vec<Item>,
    pub roots: Vec<usize>,
    /// Type path to layout record.
    pub layouts: BTreeMap<String, Ipld>,
}

pub type R<T> = Result<T, String>;

fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: Flecs returns NUL-terminated strings that live as long as the entity.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// A path segment: `\` and `.` escaped with `\`.
pub fn escape(name: &str) -> String {
    name.replace('\\', "\\\\").replace('.', "\\.")
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
fn hash8(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes)[..8].to_vec()
}

/// Sorts values by their DAG-CBOR encoding, so order never depends on Flecs ids.
fn sort_encoded(v: &mut [Ipld]) {
    v.sort_by_cached_key(encode);
}

pub struct Walk<'s> {
    w: *const ecs_world_t,
    b: Builtins,
    s: &'s Schema,
    items: Vec<Item>,
    index: HashMap<ecs_entity_t, usize>,
    paths: HashMap<ecs_entity_t, String>,
    layouts: BTreeMap<String, Ipld>,
}

impl<'s> Walk<'s> {
    pub fn new(w: *const ecs_world_t, s: &'s Schema) -> Walk<'s> {
        Walk {
            w,
            b: Builtins::read(),
            s,
            items: Vec::new(),
            index: HashMap::new(),
            paths: HashMap::new(),
            layouts: BTreeMap::new(),
        }
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
        let p = if parent == 0 {
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
            return Ok(map([("id", Ipld::Bytes(id.to_vec()))]));
        }
        Ok(map([("path", text(self.path(e)?))]))
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
        });
        self.index.insert(e, i);
        let kids = self.children(e);
        if kids.len() > MAX_CHILDREN {
            return Err(format!(
                "{path} has {} children; more than {MAX_CHILDREN} under one parent is not supported yet",
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
            let mut next = Vec::new();
            for x in layer {
                let mut bases = Vec::new();
                for id in self.ids(x) {
                    if sys::is_pair(id) && self.alive(sys::pair_first(id)) == self.b.is_a {
                        let base = self.alive(sys::pair_second(id));
                        if seen.insert(base) {
                            bases.push((self.path(base)?, base));
                        }
                    }
                }
                bases.sort();
                next.extend(bases.into_iter().map(|(_, b)| b));
            }
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
            let c =
                CString::new(*root).map_err(|_| format!("root {root:?} contains a NUL byte"))?;
            // SAFETY: plain lookup with a NUL-terminated path.
            let e = unsafe { sys::ecs_lookup(self.w, c.as_ptr()) };
            if e == 0 {
                return Err(format!("no entity named {root}"));
            }
            let path = self.path(e)?;
            let seg = path.clone();
            root_items.push(self.collect(e, None, seg, path)?);
        }
        for i in 0..self.items.len() {
            let rec = self
                .record(i)
                .map_err(|m| format!("{}: {m}", self.items[i].path))?;
            self.items[i].record = rec;
        }
        Ok(Snapshot {
            items: self.items,
            roots: root_items,
            layouts: self.layouts,
        })
    }

    fn record(&mut self, i: usize) -> R<BTreeMap<String, Ipld>> {
        let e = self.items[i].entity;
        let mut tags = Vec::new();
        let mut pairs = Vec::new();
        let mut components = BTreeMap::new();
        let (mut inh_tags, mut inh_comps, mut inh_pairs) = (Vec::new(), Vec::new(), Vec::new());
        let mut seen: HashSet<ecs_id_t> = HashSet::new();
        let mut seen_rels: HashSet<ecs_entity_t> = HashSet::new();

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
                    if self.has(comp, sys::pair(self.b.on_instantiate, self.b.dont_inherit)) {
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
                // SAFETY: plain query; the pointer is valid while the world is unchanged.
                let ptr = unsafe { sys::ecs_get_id(self.w, src, id) }.cast::<u8>();
                match part {
                    Part::Skip => {}
                    Part::Tag(t) => {
                        let r = self.reference(t)?;
                        if !own {
                            inh_tags.push(r.clone());
                        }
                        tags.push(r);
                    }
                    Part::Component(c) => {
                        let p = self.path(c)?;
                        let (hash, value) = self
                            .value(c, ptr)
                            .map_err(|m| format!("component {p}: {m}"))?;
                        if !own {
                            inh_comps.push(text(&p));
                        }
                        components.insert(p, map([("hash", hash), ("value", value)]));
                    }
                    Part::Pair(rel, tgt, ty) => {
                        seen_rels.insert(rel);
                        let (r, t) = (self.reference(rel)?, self.reference(tgt)?);
                        let mut entry = vec![r.clone(), t.clone()];
                        if let Some(ty) = ty {
                            let tp = self.path(ty)?;
                            let (hash, value) = self
                                .value(ty, ptr)
                                .map_err(|m| format!("pair with data type {tp}: {m}"))?;
                            entry.push(map([("type", text(tp)), ("hash", hash), ("value", value)]));
                        }
                        if !own {
                            inh_pairs.push(Ipld::List(vec![r, t]));
                        }
                        pairs.push(Ipld::List(entry));
                    }
                }
            }
        }
        for v in [
            &mut tags,
            &mut pairs,
            &mut inh_tags,
            &mut inh_comps,
            &mut inh_pairs,
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
        Ok(rec)
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

    /// The layout of a type (SPEC.md, "Layouts"). `rule` is the rule the enclosing member or component gives this
    /// value; struct members resolve their own.
    fn layout(&mut self, ty: ecs_entity_t) -> R<Ipld> {
        let tp = self
            .path(ty)
            .unwrap_or_else(|_| format!("#{}", ty & 0xffff_ffff));
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
                    .constants(ty)?
                    .into_iter()
                    .map(|(n, v, u)| {
                        Ipld::List(vec![
                            text(n),
                            Ipld::Integer(if bitmask {
                                v as u64 as i128
                            } else if u > i64::MAX as u64 {
                                u as i128
                            } else {
                                v as i128
                            }),
                        ])
                    })
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
                        ("underlying", self.layout(under)?),
                        ("constants", Ipld::List(list)),
                    ])
                }
            }
            sys::EcsStructType => {
                let whole = self.rule_of(ty)?;
                let mut members = Vec::new();
                for m in self.members(ty)? {
                    let mut l = self.layout(m.type_)?;
                    if m.count > 1 {
                        l = map([("array", l), ("count", Ipld::Integer(m.count as i128))]);
                    }
                    let own = if m.entity != 0 {
                        self.rule_of(m.entity)?
                    } else {
                        None
                    };
                    let nested = matches!(&l, Ipld::Map(x) if x.contains_key("struct"));
                    // A member's rule overrides the struct's; a nested struct without either merges field by field.
                    let rule = match own.or(whole) {
                        Some(r) => Some(r),
                        None if nested => None,
                        None => Some(Rule::DEFAULT),
                    };
                    if let Some(r) = rule {
                        r.check(&l)
                            .map_err(|e| format!("member {}.{}: {e}", tp, m.name))?;
                    }
                    members.push(Ipld::List(vec![
                        text(m.name),
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
                    ("array", self.layout(et)?),
                    ("count", Ipld::Integer(n as i128)),
                ])
            }
            sys::EcsVectorType => {
                let et = self
                    .get::<sys::EcsVector>(ty, self.b.vector)
                    .ok_or("no vector data")?
                    .type_;
                map([("vector", self.layout(et)?)])
            }
            sys::EcsOpaqueType => {
                return Err(format!(
                    "type {tp} is opaque (serialised by a C callback): not supported"
                ));
            }
            sys::EcsMapType => return Err(format!("type {tp} is a map: not supported")),
            _ => return Err(format!("type {tp} has an unsupported kind ({kind})")),
        })
    }

    /// The layout record of a component or pair type, made once per type: `{"layout", "merge", "writable_by",
    /// "validated_by", "hash"}`, where `merge` is the whole value's rule for a non-struct type (null for structs, whose
    /// members carry theirs) and `hash` is the first 8 bytes of SHA-256 over the record without `hash`.
    fn layout_record(&mut self, ty: ecs_entity_t) -> R<Ipld> {
        let tp = self.path(ty)?;
        if let Some(Ipld::Map(rec)) = self.layouts.get(&tp) {
            return Ok(rec["hash"].clone());
        }
        let l = self.layout(ty)?;
        let is_struct = matches!(&l, Ipld::Map(x) if x.contains_key("struct"));
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
        let mut body = BTreeMap::new();
        body.insert("layout".to_string(), l);
        body.insert("merge".to_string(), merge);
        body.insert("writable_by".to_string(), writable_by);
        body.insert("validated_by".to_string(), validated_by);
        let hash = Ipld::Bytes(hash8(&encode(&Ipld::Map(body.clone()))));
        body.insert("hash".to_string(), hash.clone());
        self.layouts.insert(tp, Ipld::Map(body));
        Ok(hash)
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

    fn members(&self, ty: ecs_entity_t) -> R<Vec<Member>> {
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
        Ok(out)
    }

    /// (layout hash, value) of a component or pair value.
    fn value(&mut self, ty: ecs_entity_t, ptr: *const u8) -> R<(Ipld, Ipld)> {
        if ptr.is_null() {
            return Err("no value".into());
        }
        let hash = self.layout_record(ty)?;
        Ok((hash, self.read(ty, ptr)?))
    }

    fn read(&mut self, ty: ecs_entity_t, ptr: *const u8) -> R<Ipld> {
        let tp = self.path(ty).unwrap_or_default();
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
                    .constants(ty)?
                    .into_iter()
                    .find(|(_, v, u)| *v as i128 == raw || *u as i128 == raw);
                match hit {
                    Some((name, ..)) => Ok(text(name)),
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
                for m in self.members(ty)? {
                    // SAFETY: member offsets are within the struct.
                    let p = unsafe { ptr.add(m.offset) };
                    let v = if m.count > 1 {
                        let stride = self.size(m.type_)?;
                        // SAFETY: an inline array of `count` elements.
                        Ipld::List(
                            (0..m.count as usize)
                                .map(|i| self.read(m.type_, unsafe { p.add(i * stride) }))
                                .collect::<R<_>>()?,
                        )
                    } else {
                        self.read(m.type_, p)?
                    };
                    entries.insert(m.name, v);
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
                        .map(|i| self.read(et, unsafe { ptr.add(i * stride) }))
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
                        .map(|i| self.read(et, unsafe { base.add(i * stride) }))
                        .collect::<R<_>>()?,
                ))
            }
            _ => self.layout(ty).map(|_| Ipld::Null), // layout() names the unsupported kind
        }
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
