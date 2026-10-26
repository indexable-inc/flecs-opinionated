//! Describes Rust types to flecs-snapshot (SPEC.md, "Described types"). One description, [`Describe`], gives a type's
//! layout `L` (runtime [`Ipld`], as SPEC.md "Layouts"), its value in a snapshot (SPEC.md "Values") and back, and the
//! shape Flecs's reflection can hold of it ([`Flecs`]). [`register`] attaches the description to a component and
//! registers (or checks) its Flecs meta, so a type, its Flecs meta and its layout record cannot disagree.
//!
//! `#[derive(Describe)]` writes the description of a struct or enum; this crate implements it for the standard types
//! and, behind features, for glam, half, bitflags (`describe_bitflags!`) and flecs_ecs's `Entity`.
//!
//! Choices this crate makes for types SPEC.md leaves open:
//! - `char` is `"u32"` (its code point); restoring a value that is not a Unicode scalar value is an error.
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
#[cfg(feature = "half")]
mod half_impls;

pub use meta::{Flecs, FlecsMember, FlecsStruct};
pub use register::{component, path_of, register};

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
}
