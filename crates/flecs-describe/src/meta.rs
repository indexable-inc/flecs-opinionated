//! Flecs meta from a description: the shape Flecs's reflection can hold of a Rust type ([`Flecs`]), registering it
//! (a struct with Rust's real offsets, an array, an enum, a bitmask) or an opaque type serialised from the description
//! when Flecs cannot express it, and checking meta a type already has against the description.
//!
//! The Flecs types this crate makes for member types, and the helper types an opaque value is serialised through,
//! live under `flecs.rust.described` (beside flecs_ecs's `flecs.rust`), so the walk treats them as builtins.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};

use flecs_c as sys;
use sys::{ecs_entity_t, ecs_world_t};

use crate::{Describe, Ipld, R, Refs};

/// What Flecs's reflection can hold of a Rust type.
#[derive(Clone, Debug, PartialEq)]
pub enum Flecs {
    /// A Flecs primitive kind (`flecs_c::EcsBool` ... `flecs_c::EcsId`) whose memory is the Rust type's.
    Primitive(i32),
    Struct(FlecsStruct),
    /// A Rust `[T; N]`.
    Array {
        name: String,
        elem: Box<Flecs>,
        count: usize,
        size: usize,
        align: usize,
    },
    /// A unit enum with an integer `#[repr]`: `underlying` is the primitive kind, constants in declaration order.
    Enum {
        name: String,
        underlying: i32,
        size: usize,
        constants: Vec<(String, i128)>,
    },
    /// A 32-bit bitmask (Flecs bitmasks are u32).
    Bitmask {
        name: String,
        constants: Vec<(String, u64)>,
    },
    /// Not expressible in Flecs's reflection (a `String`, `Vec`, `Option`, map, union, half float, runtime part, a
    /// unit enum without `#[repr]`): a component of it is an opaque type serialised from the description.
    Opaque,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlecsStruct {
    pub name: String,
    pub size: usize,
    pub align: usize,
    /// In declaration order. Zero-sized members hold no bytes and are left out of the Flecs struct.
    pub members: Vec<FlecsMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlecsMember {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub ty: Flecs,
}

impl Flecs {
    /// Whether Flecs's reflection can hold the whole value with its real memory layout.
    pub fn expressible(&self) -> bool {
        match self {
            Flecs::Primitive(_) | Flecs::Enum { .. } | Flecs::Bitmask { .. } => true,
            Flecs::Struct(s) => {
                let mut held = s.members.iter().filter(|m| m.size > 0).peekable();
                held.peek().is_some() && held.all(|m| m.ty.expressible())
            }
            Flecs::Array { elem, count, .. } => *count > 0 && elem.expressible(),
            Flecs::Opaque => false,
        }
    }

    /// What of the value Flecs's reflection can hold with its real memory layout: the whole of an expressible
    /// type; a struct's expressible members (recursively, each nested struct cut the same way), as a partial Flecs
    /// struct of the Rust size (Flecs serialises and assigns those members and leaves the rest of the bytes alone,
    /// as hand-declared partial meta did); none when no member is expressible. An array needs its element whole.
    pub fn partial(&self) -> Option<Flecs> {
        match self {
            Flecs::Struct(s) => {
                let members: Vec<FlecsMember> = s
                    .members
                    .iter()
                    .filter(|m| m.size > 0)
                    .filter_map(|m| {
                        m.ty.partial().map(|ty| FlecsMember {
                            name: m.name.clone(),
                            offset: m.offset,
                            size: m.size,
                            ty,
                        })
                    })
                    .collect();
                if members.is_empty() {
                    None
                } else {
                    Some(Flecs::Struct(FlecsStruct {
                        name: s.name.clone(),
                        size: s.size,
                        align: s.align,
                        members,
                    }))
                }
            }
            f if f.expressible() => Some(f.clone()),
            _ => None,
        }
    }

    fn kind_name(&self) -> &'static str {
        match self {
            Flecs::Primitive(_) => "primitive",
            Flecs::Struct(_) => "struct",
            Flecs::Array { .. } => "array",
            Flecs::Enum { .. } => "enum",
            Flecs::Bitmask { .. } => "bitmask",
            Flecs::Opaque => "opaque",
        }
    }
}

fn cstring(s: &str) -> R<CString> {
    CString::new(s).map_err(|_| format!("the name {s:?} holds a NUL byte"))
}

fn get<T>(w: *const ecs_world_t, e: ecs_entity_t, comp: ecs_entity_t) -> Option<&'static T> {
    // SAFETY: plain query; the component's data lives as long as the entity keeps it, which outlasts each use here.
    unsafe {
        if !sys::ecs_has_id(w, e, comp) {
            return None;
        }
        sys::ecs_get_id(w, e, comp).cast::<T>().as_ref()
    }
}

fn type_kind(w: *const ecs_world_t, e: ecs_entity_t) -> Option<i32> {
    // SAFETY: reads a C global set when the world was created.
    let id = unsafe { sys::FLECS_IDEcsTypeID_ };
    get::<sys::EcsType>(w, e, id).map(|t| t.kind)
}

/// The size and alignment of a type entity's values, if it is a component.
pub(crate) fn component_size(w: *const ecs_world_t, e: ecs_entity_t) -> Option<(usize, usize)> {
    // SAFETY: as in type_kind.
    let id = unsafe { sys::FLECS_IDEcsComponentID_ };
    get::<sys::EcsComponent>(w, e, id).map(|c| (c.size as usize, c.alignment as usize))
}

pub(crate) fn has_meta(w: *const ecs_world_t, e: ecs_entity_t) -> bool {
    type_kind(w, e).is_some()
}

fn primitive_name(k: i32) -> &'static str {
    match k {
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
        _ => "an unknown primitive",
    }
}

fn kind_name(k: i32) -> &'static str {
    match k {
        sys::EcsPrimitiveType => "primitive",
        sys::EcsBitmaskType => "bitmask",
        sys::EcsEnumType => "enum",
        sys::EcsStructType => "struct",
        sys::EcsArrayType => "array",
        sys::EcsVectorType => "vector",
        sys::EcsOpaqueType => "opaque",
        sys::EcsMapType => "map",
        _ => "unknown",
    }
}

/// A member path in front of an error, as the walk writes them.
fn at(path: &str, e: String) -> String {
    if path.is_empty() {
        e
    } else {
        crate::member_err(path, e)
    }
}

/// Registers Flecs meta for the types the description uses: `world` is live and not used by another thread.
pub(crate) struct Meta {
    pub w: *mut ecs_world_t,
    scope: ecs_entity_t,
}

impl Meta {
    /// # Safety
    ///
    /// `w` is a live world, used by no other thread while the `Meta` is.
    pub unsafe fn new(w: *mut ecs_world_t) -> R<Meta> {
        let mut parent = 0;
        for seg in ["flecs", "rust", "described"] {
            let n = cstring(seg)?;
            // SAFETY: the caller's contract; `n` is NUL-terminated.
            parent = unsafe { sys::glue_describe_type(w, parent, n.as_ptr(), 0, 0) };
            if parent == 0 {
                return Err(format!(
                    "could not make the scope entity {seg} for described types"
                ));
            }
        }
        Ok(Meta { w, scope: parent })
    }

    /// The child of the scope named `name`, a component of `size` and `align` (none when size is 0).
    fn scoped(&mut self, name: &str, size: usize, align: usize) -> R<ecs_entity_t> {
        let n = cstring(name)?;
        // SAFETY: Meta's contract.
        let e = unsafe {
            sys::glue_describe_type(self.w, self.scope, n.as_ptr(), size as i32, align as i32)
        };
        if e == 0 {
            return Err(format!("could not make the Flecs type {name}"));
        }
        if size > 0 {
            match component_size(self.w, e) {
                Some((s, a)) if s == size && a == align => {}
                Some((s, a)) => {
                    return Err(format!(
                        "the Flecs type flecs.rust.described.{name} exists with size {s} and alignment {a}, but the \
                         Rust type has {size} and {align} (two Rust types with one name?)"
                    ));
                }
                None if has_meta(self.w, e) => {}
                None => {
                    return Err(format!(
                        "flecs.rust.described.{name} exists and is not a component"
                    ));
                }
            }
        }
        Ok(e)
    }

    /// The Flecs type of an expressible member type, made (or checked) once per world.
    fn type_of(&mut self, f: &Flecs) -> R<ecs_entity_t> {
        let made = |e: ecs_entity_t, what: &str| {
            if e == 0 {
                Err(format!("Flecs refused the {what}"))
            } else {
                Ok(e)
            }
        };
        match f {
            Flecs::Primitive(k) => {
                // SAFETY: plain query.
                made(
                    unsafe { sys::glue_describe_primitive(*k) },
                    "primitive kind",
                )
            }
            Flecs::Struct(s) => {
                let e = self.scoped(&s.name, s.size, s.align)?;
                if has_meta(self.w, e) {
                    check(self.w, e, f, "").map_err(|x| format!("type {}: {x}", s.name))?;
                } else {
                    self.init_struct(e, s)?;
                }
                Ok(e)
            }
            Flecs::Array {
                name,
                elem,
                count,
                size,
                align,
            } => {
                let e = self.scoped(name, *size, *align)?;
                if has_meta(self.w, e) {
                    check(self.w, e, f, "").map_err(|x| format!("type {name}: {x}"))?;
                } else {
                    let et = self.type_of(elem)?;
                    // SAFETY: Meta's contract.
                    made(
                        unsafe { sys::glue_describe_array(self.w, e, et, *count as i32) },
                        "array type",
                    )?;
                }
                Ok(e)
            }
            Flecs::Enum { name, size, .. } => {
                let e = self.scoped(name, *size, *size)?;
                if has_meta(self.w, e) {
                    check(self.w, e, f, "").map_err(|x| format!("type {name}: {x}"))?;
                } else {
                    self.init_enum(e, f)?;
                }
                Ok(e)
            }
            Flecs::Bitmask { name, .. } => {
                let e = self.scoped(name, 4, 4)?;
                if has_meta(self.w, e) {
                    check(self.w, e, f, "").map_err(|x| format!("type {name}: {x}"))?;
                } else {
                    self.init_bitmask(e, f)?;
                }
                Ok(e)
            }
            Flecs::Opaque => Err("an opaque type has no Flecs type of its own".into()),
        }
    }

    /// Adds `s`'s members to `e` (a component of `s`'s size), one at a time with Rust's offsets.
    fn init_struct(&mut self, e: ecs_entity_t, s: &FlecsStruct) -> R<()> {
        // in offset order (Rust reorders fields; Flecs wants the member at offset 0 first).
        let mut held: Vec<&FlecsMember> = s.members.iter().filter(|m| m.size > 0).collect();
        held.sort_by_key(|m| m.offset);
        for m in held {
            // An array member is an inline array of its element (count) when the element is expressible.
            let (ty, count) = match &m.ty {
                Flecs::Array { elem, count, .. } => (self.type_of(elem)?, *count as i32),
                t => (self.type_of(t)?, 0),
            };
            let n = cstring(&m.name)?;
            // SAFETY: Meta's contract.
            let rc = unsafe {
                sys::glue_describe_member(self.w, e, n.as_ptr(), ty, count, m.offset as i32)
            };
            if rc != 0 {
                return Err(format!(
                    "Flecs refused member {} of {} (offset {}, see the Flecs log)",
                    m.name, s.name, m.offset
                ));
            }
        }
        Ok(())
    }

    fn init_enum(&mut self, e: ecs_entity_t, f: &Flecs) -> R<()> {
        let Flecs::Enum {
            name,
            underlying,
            constants,
            ..
        } = f
        else {
            return Err("not an enum".into());
        };
        // SAFETY: Meta's contract.
        let ut = unsafe { sys::glue_describe_primitive(*underlying) };
        // SAFETY: as above.
        if ut == 0 || unsafe { sys::glue_describe_enum(self.w, e, ut) } != 0 {
            return Err(format!("Flecs refused enum {name}'s underlying type"));
        }
        for (n, v) in constants {
            let c = cstring(n)?;
            // SAFETY: as above.
            let rc = unsafe {
                sys::glue_describe_enum_constant(self.w, e, c.as_ptr(), *v as i64, *v as u64)
            };
            if rc != 0 {
                return Err(format!("Flecs refused constant {n} = {v} of enum {name}"));
            }
        }
        Ok(())
    }

    fn init_bitmask(&mut self, e: ecs_entity_t, f: &Flecs) -> R<()> {
        let Flecs::Bitmask { name, constants } = f else {
            return Err("not a bitmask".into());
        };
        // SAFETY: Meta's contract.
        if unsafe { sys::glue_describe_bitmask(self.w, e) } != 0 {
            return Err(format!("Flecs refused bitmask {name}"));
        }
        for (n, v) in constants {
            // The walk skips zero constants when reading a bitmask; Flecs's bitmask leaves them out too.
            if *v == 0 {
                continue;
            }
            let bits = u32::try_from(*v)
                .map_err(|_| format!("bitmask {name}: {n} = {v:#x} does not fit 32 bits"))?;
            let c = cstring(n)?;
            // SAFETY: as above.
            unsafe { sys::glue_describe_bitmask_constant(self.w, e, c.as_ptr(), bits) };
        }
        Ok(())
    }

    /// Gives component `e` (which has no meta yet) the Flecs meta `f` describes when Flecs can express it, and
    /// returns whether it did.
    pub fn init_component(&mut self, e: ecs_entity_t, f: &Flecs) -> R<bool> {
        if !f.expressible() {
            return Ok(false);
        }
        match f {
            Flecs::Struct(s) => self.init_struct(e, s)?,
            Flecs::Array { elem, count, .. } => {
                let et = self.type_of(elem)?;
                // SAFETY: Meta's contract.
                if unsafe { sys::glue_describe_array(self.w, e, et, *count as i32) } == 0 {
                    return Err("Flecs refused the array type".into());
                }
            }
            Flecs::Enum { .. } => self.init_enum(e, f)?,
            Flecs::Bitmask { .. } => self.init_bitmask(e, f)?,
            // A component that is a bare primitive is serialised as an opaque value.
            Flecs::Primitive(_) | Flecs::Opaque => return Ok(false),
        }
        Ok(true)
    }

    /// The helper types an opaque value is serialised through, made once per world.
    fn helpers(&mut self) -> R<Helpers> {
        let size = core::mem::size_of::<Ipld>();
        let align = core::mem::align_of::<Ipld>();
        // SAFETY: plain query.
        let string = unsafe { sys::glue_describe_primitive(sys::EcsString) };
        let value = self.scoped("Value", size, align)?;
        if !has_meta(self.w, value) {
            // SAFETY: Meta's contract; the callback reads an Ipld at its pointer, which is all this type is used for.
            unsafe { sys::glue_describe_opaque(self.w, value, string, ser_value) };
        }
        let value_list = self.scoped("ValueList", 0, 0)?;
        if !has_meta(self.w, value_list) {
            // SAFETY: Meta's contract.
            unsafe { sys::glue_describe_vector(self.w, value_list, value) };
        }
        // One Value at offset 0: sized as the component so Flecs, given an explicit offset, has its alignment.
        let fields = self.scoped("Fields", size, align)?;
        if !has_meta(self.w, fields) {
            let n = cstring("value")?;
            // SAFETY: Meta's contract.
            unsafe { sys::glue_describe_member(self.w, fields, n.as_ptr(), value, 0, 0) };
        }
        let list = self.scoped("List", size, align)?;
        if !has_meta(self.w, list) {
            // SAFETY: as for Value.
            unsafe { sys::glue_describe_opaque(self.w, list, value_list, ser_list) };
        }
        let map = self.scoped("Map", size, align)?;
        if !has_meta(self.w, map) {
            // SAFETY: as for Value.
            unsafe { sys::glue_describe_opaque(self.w, map, fields, ser_map) };
        }
        for (n, e) in [
            ("Value", value),
            ("List", list),
            ("Map", map),
            ("Fields", fields),
            ("ValueList", value_list),
        ] {
            if !has_meta(self.w, e) {
                return Err(format!(
                    "Flecs refused the helper type flecs.rust.described.{n}"
                ));
            }
        }
        Ok(Helpers {
            fields,
            value_list,
            string,
        })
    }

    /// Makes component `e` opaque: Flecs (REST, JSON) shows the value T::read gives, in the shape of `layout`.
    /// Assigning it through Flecs (a script, the meta cursor) is not supported.
    pub fn init_opaque<T: Describe>(&mut self, e: ecs_entity_t, layout: &Ipld) -> R<()> {
        let h = self.helpers()?;
        let as_type = match shape(layout) {
            Shape::Map => h.fields,
            Shape::List => h.value_list,
            Shape::Value => h.string,
        };
        // SAFETY: Meta's contract; `e` holds values of T (register checked its size), which ser_component reads.
        let r = unsafe { sys::glue_describe_opaque(self.w, e, as_type, ser_component::<T>) };
        if r == 0 || !has_meta(self.w, e) {
            return Err("Flecs refused the opaque type".into());
        }
        Ok(())
    }
}

/// The as_types of an opaque component, by shape.
struct Helpers {
    fields: ecs_entity_t,
    value_list: ecs_entity_t,
    string: ecs_entity_t,
}

/// The helper types in a world, found (not made) from a serializer callback.
fn find_helpers(w: *const ecs_world_t) -> Option<(ecs_entity_t, ecs_entity_t, ecs_entity_t)> {
    let mut parent = 0;
    for seg in ["flecs", "rust", "described"] {
        let n = CString::new(seg).ok()?;
        // SAFETY: plain lookup.
        parent = unsafe { sys::ecs_lookup_child(w, parent, n.as_ptr()) };
        if parent == 0 {
            return None;
        }
    }
    let child = |name: &str| {
        let n = CString::new(name).ok()?;
        // SAFETY: plain lookup.
        let e = unsafe { sys::ecs_lookup_child(w, parent, n.as_ptr()) };
        (e != 0).then_some(e)
    };
    Some((child("Value")?, child("List")?, child("Map")?))
}

/// How an opaque value is serialised: as members (a struct, a union's one variant), as elements, or as one value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Map,
    List,
    Value,
}

fn shape(l: &Ipld) -> Shape {
    match flecs_snapshot::logical::physical(l) {
        Ipld::Map(m) if m.contains_key("struct") || m.contains_key("union") => Shape::Map,
        Ipld::Map(m)
            if ["vector", "array", "option", "map", "bitmask"]
                .iter()
                .any(|k| m.contains_key(*k)) =>
        {
            Shape::List
        }
        _ => Shape::Value,
    }
}

/// One `value()` call for `v`: a scalar as its Flecs primitive, a list or map through the helper types.
unsafe fn emit(ser: *const sys::ecs_serializer_t, v: &Ipld) -> c_int {
    // SAFETY: the serializer is live for the callback that called us.
    let w = unsafe { sys::glue_describe_ser_world(ser) };
    let Some((_, list, map)) = find_helpers(w) else {
        return -1;
    };
    let prim = |k| {
        // SAFETY: plain query.
        unsafe { sys::glue_describe_primitive(k) }
    };
    let put = |ty: ecs_entity_t, p: *const c_void| {
        // SAFETY: `p` points at a live value of `ty` for the call.
        unsafe { sys::glue_describe_ser_value(ser, ty, p) }
    };
    match v {
        Ipld::Null => {
            let p: *const c_char = core::ptr::null();
            put(prim(sys::EcsString), (&raw const p).cast())
        }
        Ipld::Bool(b) => put(prim(sys::EcsBool), core::ptr::from_ref(b).cast()),
        Ipld::Integer(i) => {
            if let Ok(x) = i64::try_from(*i) {
                put(prim(sys::EcsI64), (&raw const x).cast())
            } else if let Ok(x) = u64::try_from(*i) {
                put(prim(sys::EcsU64), (&raw const x).cast())
            } else {
                -1
            }
        }
        Ipld::Float(f) => put(prim(sys::EcsF64), core::ptr::from_ref(f).cast()),
        Ipld::String(s) => {
            let Ok(c) = CString::new(s.as_str()) else {
                return -1;
            };
            let p = c.as_ptr();
            put(prim(sys::EcsString), (&raw const p).cast())
        }
        Ipld::Link(cid) => {
            let Ok(c) = CString::new(cid.to_string()) else {
                return -1;
            };
            let p = c.as_ptr();
            put(prim(sys::EcsString), (&raw const p).cast())
        }
        Ipld::Bytes(b) => {
            let l = Ipld::List(b.iter().map(|x| Ipld::Integer(*x as i128)).collect());
            put(list, (&raw const l).cast())
        }
        Ipld::List(_) => put(list, core::ptr::from_ref(v).cast()),
        Ipld::Map(_) => put(map, core::ptr::from_ref(v).cast()),
    }
}

unsafe fn emit_elements(ser: *const sys::ecs_serializer_t, items: &[Ipld]) -> c_int {
    for x in items {
        // SAFETY: the caller's serializer.
        let rc = unsafe { emit(ser, x) };
        if rc != 0 {
            return rc;
        }
    }
    0
}

unsafe fn emit_members(ser: *const sys::ecs_serializer_t, m: &BTreeMap<String, Ipld>) -> c_int {
    for (k, x) in m {
        let Ok(n) = CString::new(k.as_str()) else {
            return -1;
        };
        // SAFETY: the caller's serializer; `n` lives for the call.
        let rc = unsafe { sys::glue_describe_ser_member(ser, n.as_ptr()) };
        if rc != 0 {
            return rc;
        }
        // SAFETY: as above.
        let rc = unsafe { emit(ser, x) };
        if rc != 0 {
            return rc;
        }
    }
    0
}

unsafe extern "C" fn ser_value(ser: *const sys::ecs_serializer_t, src: *const c_void) -> c_int {
    // SAFETY: the helper types are only ever serialised from `emit`, with a pointer to an Ipld.
    unsafe { emit(ser, &*src.cast::<Ipld>()) }
}

unsafe extern "C" fn ser_list(ser: *const sys::ecs_serializer_t, src: *const c_void) -> c_int {
    // SAFETY: as in ser_value.
    match unsafe { &*src.cast::<Ipld>() } {
        // SAFETY: as above.
        Ipld::List(items) => unsafe { emit_elements(ser, items) },
        _ => -1,
    }
}

unsafe extern "C" fn ser_map(ser: *const sys::ecs_serializer_t, src: *const c_void) -> c_int {
    // SAFETY: as in ser_value.
    match unsafe { &*src.cast::<Ipld>() } {
        // SAFETY: as above.
        Ipld::Map(m) => unsafe { emit_members(ser, m) },
        _ => -1,
    }
}

/// Refs for display (REST, JSON): an entity's Flecs path, or `#id` when it has none.
struct PathRefs(*const ecs_world_t);

impl Refs for PathRefs {
    fn reference(&mut self, e: ecs_entity_t) -> R<Ipld> {
        // SAFETY: the world the serializer was called for, live during the call.
        Ok(Ipld::String(
            unsafe { crate::path_of(self.0, e) }
                .unwrap_or_else(|_| format!("#{}", e & 0xffff_ffff)),
        ))
    }
}

/// The serialize callback of a component of described type T.
unsafe extern "C" fn ser_component<T: Describe>(
    ser: *const sys::ecs_serializer_t,
    src: *const c_void,
) -> c_int {
    // SAFETY: Flecs calls this with a live value of the component, which holds a T (register checked its size).
    let t = unsafe { &*src.cast::<T>() };
    // SAFETY: the serializer is live for the call.
    let w = unsafe { sys::glue_describe_ser_world(ser) };
    let Ok(v) = t.read(&mut PathRefs(w)) else {
        return -1;
    };
    // SAFETY: as above.
    unsafe {
        match (shape(&T::layout()), &v) {
            (Shape::Map, Ipld::Map(m)) => emit_members(ser, m),
            (Shape::List, Ipld::List(items)) => emit_elements(ser, items),
            (Shape::List, Ipld::Bytes(b)) => emit_elements(
                ser,
                &b.iter()
                    .map(|x| Ipld::Integer(*x as i128))
                    .collect::<Vec<_>>(),
            ),
            (Shape::Value, x) => emit(ser, x),
            _ => -1,
        }
    }
}

// ---- Checking meta a type already has ----

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: Flecs's names are NUL-terminated and live as long as the type.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

struct Member {
    name: String,
    ty: ecs_entity_t,
    count: i32,
    offset: usize,
}

fn members(w: *const ecs_world_t, e: ecs_entity_t) -> R<Vec<Member>> {
    // SAFETY: a C global.
    let id = unsafe { sys::FLECS_IDEcsStructID_ };
    let s = get::<sys::EcsStruct>(w, e, id).ok_or("no struct data")?;
    let v = &s.members;
    Ok((0..v.count as usize)
        .map(|i| {
            // SAFETY: the vector holds `count` ecs_member_t.
            let m = unsafe { &*v.array.cast::<sys::ecs_member_t>().add(i) };
            Member {
                name: cstr(m.name),
                ty: m.type_,
                count: m.count,
                offset: m.offset as usize,
            }
        })
        .collect())
}

/// (name, signed, unsigned) constants in registration order.
fn constants(w: *const ecs_world_t, e: ecs_entity_t) -> R<Vec<(String, i64, u64)>> {
    // SAFETY: a C global.
    let id = unsafe { sys::FLECS_IDEcsConstantsID_ };
    let c = get::<sys::EcsConstants>(w, e, id).ok_or("no constants")?;
    let v = &c.ordered_constants;
    Ok((0..v.count as usize)
        .map(|i| {
            // SAFETY: the vector holds `count` ecs_enum_constant_t (bitmask constants share the layout).
            let k = unsafe { &*v.array.cast::<sys::ecs_enum_constant_t>().add(i) };
            (cstr(k.name), k.value, k.value_unsigned)
        })
        .collect())
}

fn primitive_kind(w: *const ecs_world_t, e: ecs_entity_t) -> Option<i32> {
    // SAFETY: a C global.
    let id = unsafe { sys::FLECS_IDEcsPrimitiveID_ };
    get::<sys::EcsPrimitive>(w, e, id).map(|p| p.kind)
}

fn same_primitive(a: i32, b: i32) -> bool {
    let norm = |k| if k == sys::EcsByte { sys::EcsU8 } else { k };
    norm(a) == norm(b)
}

/// Checks the Flecs meta of type `e` against `f`: member names, order and offsets, primitive kinds, enum and bitmask
/// constants, array counts. A type either side holds as opaque is not looked into (its shape is the host's or the
/// description's to say). `path` is the member path so far, for the error.
pub(crate) fn check(w: *const ecs_world_t, e: ecs_entity_t, f: &Flecs, path: &str) -> R<()> {
    let kind = type_kind(w, e).ok_or_else(|| at(path, "has no Flecs reflection data".into()))?;
    if kind == sys::EcsOpaqueType || *f == Flecs::Opaque {
        return Ok(());
    }
    match (kind, f) {
        (sys::EcsPrimitiveType, Flecs::Primitive(k)) => {
            let have = primitive_kind(w, e).ok_or_else(|| at(path, "no primitive kind".into()))?;
            if same_primitive(have, *k) {
                Ok(())
            } else {
                Err(at(
                    path,
                    format!("is {} in Flecs where the Rust type is {}", primitive_name(have), primitive_name(*k)),
                ))
            }
        }
        (sys::EcsStructType, Flecs::Struct(s)) => {
            // both in offset order (Flecs holds members in the order they were declared, hand-written meta
            // in offset order, the derive in declaration order).
            let mut have = members(w, e).map_err(|x| at(path, x))?;
            have.sort_by_key(|m| m.offset);
            let mut want: Vec<&FlecsMember> = s.members.iter().filter(|m| m.size > 0).collect();
            want.sort_by_key(|m| m.offset);
            for (i, m) in want.iter().enumerate() {
                let Some(h) = have.get(i) else {
                    return Err(at(path, format!("Flecs has no member {} (the Rust type's member {i})", m.name)));
                };
                if h.name != m.name {
                    return Err(at(
                        path,
                        format!("member {i} is {} in Flecs where the Rust type has {}", h.name, m.name),
                    ));
                }
                let mp = if path.is_empty() { m.name.clone() } else { format!("{path}.{}", m.name) };
                if h.offset != m.offset {
                    return Err(crate::member_err(
                        &mp,
                        format!("is at offset {} in Flecs where Rust puts it at {}", h.offset, m.offset),
                    ));
                }
                if h.count > 1 {
                    match &m.ty {
                        Flecs::Array { elem, count, .. } if *count == h.count as usize => check(w, h.ty, elem, &mp)?,
                        Flecs::Opaque => {}
                        t => {
                            return Err(crate::member_err(
                                &mp,
                                format!("is an inline array of {} in Flecs where the Rust type is a {}", h.count, t.kind_name()),
                            ));
                        }
                    }
                } else {
                    check(w, h.ty, &m.ty, &mp)?;
                }
            }
            if have.len() > want.len() {
                return Err(at(
                    path,
                    format!("Flecs has the member {} that the Rust type does not", have[want.len()].name),
                ));
            }
            Ok(())
        }
        (sys::EcsArrayType, Flecs::Array { elem, count, .. }) => {
            // SAFETY: a C global.
            let id = unsafe { sys::FLECS_IDEcsArrayID_ };
            let a = get::<sys::EcsArray>(w, e, id).ok_or_else(|| at(path, "no array data".into()))?;
            if a.count as usize != *count {
                return Err(at(path, format!("is an array of {} in Flecs where Rust's has {count}", a.count)));
            }
            check(w, a.type_, elem, &format!("{path}[]"))
        }
        (sys::EcsEnumType, Flecs::Enum { underlying, constants: want, .. }) => {
            // SAFETY: a C global.
            let id = unsafe { sys::FLECS_IDEcsEnumID_ };
            let en = get::<sys::EcsEnum>(w, e, id).ok_or_else(|| at(path, "no enum data".into()))?;
            let uk = primitive_kind(w, en.underlying_type).unwrap_or(-1);
            if !same_primitive(uk, *underlying) {
                return Err(at(
                    path,
                    format!(
                        "is an enum over {} in Flecs where the Rust type's repr is {}",
                        primitive_name(uk),
                        primitive_name(*underlying)
                    ),
                ));
            }
            let unsigned = matches!(uk, sys::EcsU8 | sys::EcsU16 | sys::EcsU32 | sys::EcsU64 | sys::EcsUPtr | sys::EcsByte);
            let have: Vec<(String, i128)> = constants(w, e)?
                .into_iter()
                .map(|(n, v, u)| (n, if unsigned { u as i128 } else { v as i128 }))
                .collect();
            if have != *want {
                return Err(at(path, format!("has the constants {have:?} in Flecs where the Rust type has {want:?}")));
            }
            Ok(())
        }
        (sys::EcsBitmaskType, Flecs::Bitmask { constants: want, .. }) => {
            let have: Vec<(String, u64)> = constants(w, e)?
                .into_iter()
                .map(|(n, v, _)| (n, v as u64))
                .filter(|(_, v)| *v != 0)
                .collect();
            let want: Vec<(String, u64)> = want.iter().filter(|(_, v)| *v != 0).cloned().collect();
            if have != want {
                return Err(at(path, format!("has the constants {have:?} in Flecs where the Rust type has {want:?}")));
            }
            Ok(())
        }
        (sys::EcsVectorType, _) => Err(at(
            path,
            "is a Flecs vector (an ecs_vec_t), which no Rust type here is laid out as; register a Rust Vec as an \
             opaque type"
                .into(),
        )),
        (k, f) => Err(at(
            path,
            format!("is a Flecs {} where the Rust type is a {}", kind_name(k), f.kind_name()),
        )),
    }
}
