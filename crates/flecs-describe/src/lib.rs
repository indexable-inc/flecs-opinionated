//! Describes Rust types to flecs-snapshot (SPEC.md, "Described types"). One description, [`Describe`], gives a type's
//! layout `L` (runtime [`Ipld`], as SPEC.md "Layouts"), its value in a snapshot (SPEC.md "Values") and back, and the
//! shape Flecs's reflection can hold of it ([`Flecs`]). [`register`] attaches the description to a component and
//! registers (or checks) its Flecs meta, so a type, its Flecs meta and its layout record cannot disagree.
//!
//! `#[derive(Describe)]` writes the description of a struct or enum; this crate implements it for the standard types
//! and, behind features, for glam, glamx (rapier's poses), half, bitflags (`describe_bitflags!`) and flecs_ecs's `Entity`.
//!
//! Choices this crate makes for types SPEC.md leaves open:
//! - `char` is `"u32"` (its code point); restoring a value that is not a Unicode scalar value is an error.
//! - `NonZero<T>` (each integer kind but the 128-bit ones) is the logical type `NonZero<T>` over `T` (SPEC.md,
//!   "Logical types"): `{"logical": "NonZero<u32>", "of": "u32", "rules": {"range": [1, null]}}` for an unsigned `T`,
//!   the check `sync.checks.NonZero` for a signed one; its value is the bare integer, its hash differs from `T`'s,
//!   and restoring a zero is refused. Opaque to Flecs (shown, not writable), so Flecs Script and REST cannot write a
//!   zero into one.
//! - `&'static T` for a [`Catalog`] `T` is saved as the entry's name (`refers_to` the catalog).
//! - The derive's refinements: `#[describe(range(min = .., max = ..))]`, `unit = "path"`, `finite` on a field (or on a
//!   `transparent` type with `logical = "Name"`), and `validate = path` on a type (a `fn(&Self) -> Result<(), String>`
//!   run by `from_value` and, as the check `sync.checks.<type name>`, by every restore and `set`). The library checks
//!   a value against its rules before `from_value` sees it; `from_value` itself checks only `validate`.
//! - `u128` is `{"struct": "u128", "members": [["hi", "u64"], ["lo", "u64"]]}` and `i128` the same with `hi` an `i64`
//!   (two's complement halves), since DAG-CBOR integers stop at 64 bits.
//! - `()` and `PhantomData<T>` are structs with no members (`{"struct": null, "members": []}`).
//! - `HashSet` and `BTreeSet` are `{"vector": K}` whose value is sorted by the elements' DAG-CBOR, with no duplicate;
//!   restoring a list with a duplicate is an error.
//! - `Box<T>`, `Arc<T>` and `Rc<T>` are described as `T`; restoring makes a new box. `VecDeque<T>` is a vector, front
//!   first. `Result<T, E>` is a union with variants `Ok` and `Err`, each a struct with member `0`. `PathBuf` is a
//!   string (a path that is not UTF-8 is an error).
//! - `f32` (and `f16`, `bf16`) refuse a float they cannot hold exactly; a float in memory that is NaN or infinite is
//!   refused when read, as DAG-CBOR has neither.
//! - A unit enum's `underlying` is its `#[repr]` (`u8` ... `i64`, `usize` as `uptr`, `isize` as `iptr`, `C` as `i32`),
//!   and `i64` without one. Only an enum with an integer `#[repr]` becomes a Flecs enum; without one Rust's layout is
//!   unspecified, so Flecs sees it as an opaque type.

use std::collections::BTreeMap;

pub use flecs_describe_derive::Describe;
pub use flecs_snapshot::described::{Refs, Resolve};
pub use ipld_core::ipld::Ipld;

mod catalog;
mod impls;
mod meta;
mod register;
pub mod with;

#[cfg(feature = "bitflags")]
mod bits;
#[cfg(feature = "flecs_ecs")]
mod ecs;
#[cfg(feature = "glam")]
mod glam_impls;
#[cfg(feature = "glamx")]
mod glamx_impls;
#[cfg(feature = "half")]
mod half_impls;

pub use catalog::Catalog;
pub use meta::{Flecs, FlecsMember, FlecsStruct};
pub use register::{Swept, component, describe_all, path_of, register};

pub type R<T> = Result<T, String>;

/// A Rust type as flecs-snapshot sees it.
///
/// `read` and `from_value` are inverse: `from_value(read(x)) == x` for every value, so a snapshot restores exactly what
/// was saved (a `runtime` part excepted: it restores its default). `from_value` is given a value already checked
/// against `layout`, but checks again what it relies on and returns an error rather than panicking.
pub trait Describe: Sized + 'static {
    /// The name used in this type's layout (a struct's, enum's, union's or bitmask's path) and for the Flecs type made
    /// for it when it is a member of another type. The bare Rust name for a derived type, with its type arguments'
    /// names for a generic one (`Pair<f32>`).
    fn type_name() -> String;

    /// The layout `L` (SPEC.md, "Layouts"). Struct members' rule slots hold the declared rule or null.
    fn layout() -> Ipld;

    /// The value (SPEC.md, "Values").
    fn read(&self, refs: &mut dyn Refs) -> R<Ipld>;

    /// The Rust value of a value in this layout.
    fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self>;

    /// What Flecs's reflection can hold of the type: its real memory layout when Flecs can express it.
    fn flecs() -> Flecs {
        Flecs::Opaque
    }
}

/// Puts a member (or `[i]` element) in front of an error's member path: `member pos.x: ...`, `member pts[2].x: ...`,
/// as flecs-snapshot's walk writes them.
pub fn member_err(name: &str, e: String) -> String {
    match e.strip_prefix("member ") {
        Some(rest) if rest.starts_with('[') => format!("member {name}{rest}"),
        Some(rest) => format!("member {name}.{rest}"),
        None => format!("member {name}: {e}"),
    }
}

/// The prefix of a runtime part's type text when the derive made it because the member's type
/// has no description (not marked `#[describe(runtime)]`): `{"runtime": "implicit:<type>"}`. A
/// check over saved components refuses those.
pub const IMPLICIT_RUNTIME: &str = "implicit:";

/// What the derive and the macros call. Not a stable interface.
#[doc(hidden)]
pub mod __private {
    use super::*;

    #[cfg(feature = "bitflags")]
    pub use crate::bits::{
        bitmask_flecs, bitmask_from_value, bitmask_layout, bitmask_read, short_name,
    };
    pub use crate::member_err;
    pub use crate::meta::{Flecs, FlecsMember, FlecsStruct};
    pub use core::mem::{align_of, offset_of, size_of};
    /// The Flecs primitive kinds (`sys::EcsU8` ...) a derived enum's `Flecs::Enum` names.
    pub use flecs_c as sys;

    pub fn text(s: impl Into<String>) -> Ipld {
        Ipld::String(s.into())
    }

    /// `{"struct": name, "members": [[name, L, rule | null], ...]}`.
    pub fn struct_layout(name: Option<String>, members: Vec<(String, Ipld, Option<&str>)>) -> Ipld {
        let members = members
            .into_iter()
            .map(|(n, l, r)| Ipld::List(vec![text(n), l, r.map_or(Ipld::Null, text)]))
            .collect();
        let mut m = BTreeMap::new();
        m.insert("struct".to_string(), name.map_or(Ipld::Null, Ipld::String));
        m.insert("members".to_string(), Ipld::List(members));
        Ipld::Map(m)
    }

    /// `{"union": name, "variants": [[name, payload | null], ...]}`.
    pub fn union_layout(name: String, variants: Vec<(String, Option<Ipld>)>) -> Ipld {
        let variants = variants
            .into_iter()
            .map(|(n, p)| Ipld::List(vec![text(n), p.unwrap_or(Ipld::Null)]))
            .collect();
        let mut m = BTreeMap::new();
        m.insert("union".to_string(), Ipld::String(name));
        m.insert("variants".to_string(), Ipld::List(variants));
        Ipld::Map(m)
    }

    /// `{"enum": name, "underlying": L, "constants": [[name, int], ...]}`.
    pub fn enum_layout(name: String, underlying: &str, constants: Vec<(&str, i128)>) -> Ipld {
        let constants = constants
            .into_iter()
            .map(|(n, v)| Ipld::List(vec![text(n), Ipld::Integer(v)]))
            .collect();
        let mut m = BTreeMap::new();
        m.insert("enum".to_string(), Ipld::String(name));
        m.insert("underlying".to_string(), text(underlying));
        m.insert("constants".to_string(), Ipld::List(constants));
        Ipld::Map(m)
    }

    /// `{"runtime": path}`.
    pub fn runtime_layout(path: impl Into<String>) -> Ipld {
        let mut m = BTreeMap::new();
        m.insert("runtime".to_string(), text(path));
        Ipld::Map(m)
    }

    pub fn struct_value(entries: Vec<(&str, Ipld)>) -> Ipld {
        Ipld::Map(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    pub fn union_value(name: &str, payload: Option<Ipld>) -> Ipld {
        let mut m = BTreeMap::new();
        m.insert(name.to_string(), payload.unwrap_or(Ipld::Null));
        Ipld::Map(m)
    }

    /// A struct value with exactly the members `names`.
    pub fn take_struct<'a>(v: &'a Ipld, names: &[&str]) -> R<&'a BTreeMap<String, Ipld>> {
        let Ipld::Map(m) = v else {
            return Err("a struct value is not a map".into());
        };
        for k in m.keys() {
            if !names.contains(&k.as_str()) {
                return Err(format!("the struct value has the unknown member {k:?}"));
            }
        }
        for n in names {
            if !m.contains_key(*n) {
                return Err(format!("the struct value has no member {n:?}"));
            }
        }
        Ok(m)
    }

    /// A member `take_struct` checked is there.
    pub fn get<'a>(m: &'a BTreeMap<String, Ipld>, name: &str) -> R<&'a Ipld> {
        m.get(name)
            .ok_or_else(|| format!("the struct value has no member {name:?}"))
    }

    /// A union value: a map with exactly one key, the variant's name.
    pub fn take_union(v: &Ipld) -> R<(&str, &Ipld)> {
        match v {
            Ipld::Map(m) if m.len() == 1 => {
                let (k, p) = m.iter().next().expect("one entry");
                Ok((k.as_str(), p))
            }
            _ => Err("a union value is not a map with exactly one key (the variant's name)".into()),
        }
    }

    pub fn expect_null(v: &Ipld, what: &str) -> R<()> {
        match v {
            Ipld::Null => Ok(()),
            _ => Err(format!("{what} holds a value where null was expected")),
        }
    }

    pub fn text_of<'a>(v: &'a Ipld, what: &str) -> R<&'a str> {
        match v {
            Ipld::String(s) => Ok(s),
            _ => Err(format!("{what} is not text")),
        }
    }

    pub fn unknown_variant(name: &str, ty: &str) -> String {
        format!("{name:?} is not a variant of {ty}")
    }

    pub fn unknown_constant(name: &str, ty: &str) -> String {
        format!("{name:?} is not a constant of enum {ty}")
    }

    /// A runtime part's value: always null; restoring leaves the default.
    pub fn runtime_from_value<T: Default>(v: &Ipld) -> R<T> {
        expect_null(v, "a runtime part")?;
        Ok(T::default())
    }

    /// The offset of a field reached through `Deref` (glam's SIMD types), measured on a live value.
    pub fn offset_in<S, F>(s: &S, f: &F) -> usize {
        (core::ptr::from_ref(f) as usize) - (core::ptr::from_ref(s) as usize)
    }

    /// The name of a type as written (`a::b::Foo` gives `Foo`), spaces removed.
    pub fn type_text(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    // What the derive's code reaches a member's type through, so a member whose type has no description (a
    // foreign type, a handle, a channel) is a runtime part instead of a compile error. Autoref specialisation:
    // `(&&Probe::<T>(PhantomData)).p_layout()` picks `ViaDescribe` when `T: Describe` and `ViaRuntime` otherwise.
    // In a generic type the choice is made with the type's own bounds (each type parameter `: Describe`).

    pub use linkme;

    /// A type to probe; never holds a value.
    pub struct Probe<T>(pub core::marker::PhantomData<T>);

    pub trait ViaDescribe<T> {
        fn p_layout(&self) -> Ipld;
        fn p_read(&self, v: &T, refs: &mut dyn Refs) -> R<Ipld>;
        /// `default` is the expansion site's `Default` probe (a generic function cannot probe for it).
        fn p_from(&self, v: &Ipld, ids: &mut dyn Resolve, default: fn() -> R<T>) -> R<T>;
        fn p_flecs(&self) -> Flecs;
    }

    impl<T: Describe> ViaDescribe<T> for &Probe<T> {
        fn p_layout(&self) -> Ipld {
            T::layout()
        }
        fn p_read(&self, v: &T, refs: &mut dyn Refs) -> R<Ipld> {
            v.read(refs)
        }
        fn p_from(&self, v: &Ipld, ids: &mut dyn Resolve, _default: fn() -> R<T>) -> R<T> {
            T::from_value(v, ids)
        }
        fn p_flecs(&self) -> Flecs {
            T::flecs()
        }
    }

    pub trait ViaRuntime<T> {
        fn p_layout(&self) -> Ipld;
        fn p_read(&self, v: &T, refs: &mut dyn Refs) -> R<Ipld>;
        /// `default` is the expansion site's `Default` probe (a generic function cannot probe for it).
        fn p_from(&self, v: &Ipld, ids: &mut dyn Resolve, default: fn() -> R<T>) -> R<T>;
        fn p_flecs(&self) -> Flecs;
    }

    impl<T> ViaRuntime<T> for Probe<T> {
        fn p_layout(&self) -> Ipld {
            runtime_layout(format!(
                "{}{}",
                crate::IMPLICIT_RUNTIME,
                type_text(core::any::type_name::<T>())
            ))
        }
        fn p_read(&self, _v: &T, _refs: &mut dyn Refs) -> R<Ipld> {
            Ok(Ipld::Null)
        }
        fn p_from(&self, v: &Ipld, _ids: &mut dyn Resolve, default: fn() -> R<T>) -> R<T> {
            expect_null(v, "a runtime part")?;
            default()
        }
        fn p_flecs(&self) -> Flecs {
            Flecs::Opaque
        }
    }

    /// A type whose `Default` a runtime part is restored as, when it has one.
    pub struct DefaultProbe<T>(pub core::marker::PhantomData<T>);

    pub trait ViaDefault<T> {
        fn p_default(&self) -> R<T>;
    }

    impl<T: Default> ViaDefault<T> for &DefaultProbe<T> {
        fn p_default(&self) -> R<T> {
            Ok(T::default())
        }
    }

    pub trait ViaNoDefault<T> {
        fn p_default(&self) -> R<T>;
    }

    impl<T> ViaNoDefault<T> for DefaultProbe<T> {
        fn p_default(&self) -> R<T> {
            Err(format!(
                "{} has no Default, so a runtime part holding it cannot be restored",
                core::any::type_name::<T>()
            ))
        }
    }

    /// One derived (non-generic) type: its Rust symbol (`type_name`, flecs_ecs's component symbol) and what
    /// registers its description on the component of that symbol.
    pub struct Entry {
        pub symbol: fn() -> &'static str,
        pub register: unsafe fn(*mut flecs_c::ecs_world_t, flecs_c::ecs_entity_t) -> R<()>,
    }

    /// Every derived non-generic type in the process.
    #[linkme::distributed_slice]
    pub static DESCRIBED: [Entry];

    /// A range bound as the derive writes it (`range(min = 1)`): converted to the physical type's kind by
    /// [`refine`].
    pub enum Bound {
        None,
        I(i128),
        F(f64),
    }

    impl Bound {
        fn ipld(self) -> Ipld {
            match self {
                Bound::None => Ipld::Null,
                Bound::I(i) => Ipld::Integer(i),
                Bound::F(f) => Ipld::Float(f),
            }
        }
    }

    /// `("range", [min, max])`.
    pub fn range_rule(min: Bound, max: Bound) -> (&'static str, Ipld) {
        ("range", Ipld::List(vec![min.ipld(), max.ipld()]))
    }

    /// `inner` refined by `rules` (SPEC.md, "Logical types"), folded into one node when `inner` is already refined.
    /// A rule that does not fit the physical type (a range on a string, two units) is a bug in the type's
    /// attributes: it panics with the reason when the layout is first made, which registering the type does.
    pub fn refine(inner: Ipld, name: Option<String>, rules: Vec<(&'static str, Ipld)>) -> Ipld {
        let rules = rules.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        flecs_snapshot::logical::node(name, inner, rules)
            .unwrap_or_else(|e| panic!("a described type's refinement is invalid: {e}"))
    }

    /// The check entity of a derived type's `validate` function: `sync.checks.<type name>`.
    pub fn check_path(type_name: &str) -> String {
        format!("sync.checks.{}", crate::register::escape(type_name))
    }

    /// A check's resolver: a ref reads as no entity (0), as a check sees values, not the world.
    pub struct NoIds;

    impl Resolve for NoIds {
        fn entity(&mut self, _: &Ipld) -> R<flecs_c::ecs_entity_t> {
            Ok(0)
        }
    }

    /// One `validate` check: its entity path and its function over a value in its snapshot shape.
    pub struct Check {
        pub path: fn() -> String,
        pub check: flecs_snapshot::logical::CheckFn,
    }

    /// Every derived type's `validate` check in the process; [`crate::register`] registers them all in the world
    /// first.
    #[linkme::distributed_slice]
    pub static CHECKS: [Check];

    /// A derived type's check: decodes the value as `T`, whose `from_value` runs the type's `validate`.
    pub fn check_by_decode<T: Describe>(v: &Ipld) -> R<()> {
        T::from_value(v, &mut NoIds).map(|_| ())
    }

    /// `register::<T>` as a plain function pointer for an [`Entry`].
    ///
    /// # Safety
    ///
    /// As [`crate::register`].
    pub unsafe fn register_entry<T: Describe>(
        world: *mut flecs_c::ecs_world_t,
        component: flecs_c::ecs_entity_t,
    ) -> R<()> {
        // SAFETY: the caller's contract.
        unsafe { crate::register::<T>(world, component) }
    }
}
