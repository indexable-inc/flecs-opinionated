//! bitflags types as bitmasks: `describe_bitflags!(Type)` (or `describe_bitflags!(Type, "name")`) in the crate that
//! defines `Type`.
//!
//! The layout is `{"bitmask": name, "constants": [[name, uint], ...]}` with the named, non-zero flags in declaration
//! order (a signed flag type's values are its bit patterns as unsigned of the same width). The value is the list of
//! constant names read as flecs-snapshot reads a Flecs bitmask: in declaration order, each flag whose bits are all
//! still set is listed and its bits cleared; bits left over are an error. A flag type whose bits are 32 bits wide is a
//! Flecs bitmask (Flecs bitmasks are u32); any other width is opaque to Flecs.

use bitflags::Flags;

use crate::__private::text;
use crate::{Flecs, Ipld, R};

/// The integer types bitflags can use, as their bit patterns.
pub trait BitsInt: Copy {
    const WIDTH: u32;
    fn to_u64(self) -> u64;
    /// `None` when `v` has bits above the width.
    fn from_u64(v: u64) -> Option<Self>;
}

macro_rules! bits_int {
    ($($t:ty => $u:ty),*) => {$(
        impl BitsInt for $t {
            const WIDTH: u32 = <$u>::BITS;
            fn to_u64(self) -> u64 {
                self as $u as u64
            }
            fn from_u64(v: u64) -> Option<Self> {
                <$u>::try_from(v).ok().map(|x| x as $t)
            }
        }
    )*};
}

bits_int!(u8 => u8, u16 => u16, u32 => u32, u64 => u64, i8 => u8, i16 => u16, i32 => u32, i64 => u64);

/// The named, non-zero flags in declaration order.
fn flags<F: Flags>() -> Vec<(&'static str, u64)>
where
    F::Bits: BitsInt,
{
    F::FLAGS
        .iter()
        .filter(|f| !f.name().is_empty())
        .map(|f| (f.name(), f.value().bits().to_u64()))
        .filter(|(_, v)| *v != 0)
        .collect()
}

/// The last segment of a type as written: `crate::m::Flags` gives `Flags`.
pub fn short_name(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    s.rsplit("::").next().unwrap_or(&s).to_string()
}

pub fn bitmask_layout<F: Flags>(name: String) -> Ipld
where
    F::Bits: BitsInt,
{
    let constants = flags::<F>()
        .into_iter()
        .map(|(n, v)| Ipld::List(vec![text(n), Ipld::Integer(v as i128)]))
        .collect();
    let mut m = std::collections::BTreeMap::new();
    m.insert("bitmask".to_string(), Ipld::String(name));
    m.insert("constants".to_string(), Ipld::List(constants));
    Ipld::Map(m)
}

pub fn bitmask_read<F: Flags>(x: &F) -> R<Ipld>
where
    F::Bits: BitsInt,
{
    let mut rest = x.bits().to_u64();
    let mut names = Vec::new();
    for (n, v) in flags::<F>() {
        if rest & v == v {
            names.push(text(n));
            rest &= !v;
        }
    }
    if rest != 0 {
        return Err(format!("bits {rest:#x} are not named flags"));
    }
    Ok(Ipld::List(names))
}

pub fn bitmask_from_value<F: Flags>(v: &Ipld) -> R<F>
where
    F::Bits: BitsInt,
{
    let Ipld::List(names) = v else {
        return Err("a bitmask value is not a list".into());
    };
    let all = flags::<F>();
    let mut bits = 0u64;
    let mut seen: Vec<&str> = Vec::new();
    for n in names {
        let Ipld::String(n) = n else {
            return Err("a bitmask constant is not text".into());
        };
        if seen.contains(&n.as_str()) {
            return Err(format!("{n:?} is listed twice in a bitmask value"));
        }
        seen.push(n.as_str());
        let (_, b) = all
            .iter()
            .find(|(c, _)| *c == n.as_str())
            .ok_or_else(|| format!("{n:?} is not a flag"))?;
        bits |= b;
    }
    let b = <F::Bits as BitsInt>::from_u64(bits)
        .ok_or_else(|| format!("bits {bits:#x} do not fit the flag type"))?;
    F::from_bits(b).ok_or_else(|| format!("bits {bits:#x} are not valid flags"))
}

pub fn bitmask_flecs<F: Flags>(name: String) -> Flecs
where
    F::Bits: BitsInt,
{
    if <F::Bits as BitsInt>::WIDTH != 32 || core::mem::size_of::<F>() != 4 {
        return Flecs::Opaque;
    }
    Flecs::Bitmask {
        name,
        constants: flags::<F>()
            .into_iter()
            .map(|(n, v)| (n.to_string(), v))
            .collect(),
    }
}

/// Describes a bitflags type as a bitmask (see the module docs). The name defaults to the type's last path segment.
#[macro_export]
macro_rules! describe_bitflags {
    ($t:ty) => {
        $crate::describe_bitflags!($t, $crate::__private::short_name(stringify!($t)));
    };
    ($t:ty, $name:expr) => {
        impl $crate::Describe for $t {
            fn type_name() -> ::std::string::String {
                ::std::string::ToString::to_string(&$name)
            }
            fn layout() -> $crate::Ipld {
                $crate::__private::bitmask_layout::<$t>(<$t as $crate::Describe>::type_name())
            }
            fn read(&self, _: &mut dyn $crate::Refs) -> $crate::R<$crate::Ipld> {
                $crate::__private::bitmask_read(self)
            }
            fn from_value(v: &$crate::Ipld, _: &mut dyn $crate::Resolve) -> $crate::R<Self> {
                $crate::__private::bitmask_from_value(v)
            }
            fn flecs() -> $crate::Flecs {
                $crate::__private::bitmask_flecs::<$t>(<$t as $crate::Describe>::type_name())
            }
        }
    };
}
