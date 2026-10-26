//! Describe for the standard types (the choices are listed in the crate docs).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hash};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use flecs_c as sys;

use crate::__private::{
    get, struct_layout, struct_value, take_struct, take_union, text, union_layout, union_value,
};
use crate::{Describe, Flecs, FlecsMember, FlecsStruct, Ipld, R, Refs, Resolve, member_err};

fn single(key: &str, v: Ipld) -> Ipld {
    let mut m = BTreeMap::new();
    m.insert(key.to_string(), v);
    Ipld::Map(m)
}

/// A finite float as a value (DAG-CBOR has no NaN or infinities).
pub(crate) fn float(v: f64) -> R<Ipld> {
    if v.is_finite() {
        Ok(Ipld::Float(v))
    } else {
        Err(format!(
            "{v} cannot be stored: DAG-CBOR allows only finite floats"
        ))
    }
}

pub(crate) fn float_of(v: &Ipld, what: &str) -> R<f64> {
    match v {
        Ipld::Float(f) if f.is_finite() => Ok(*f),
        _ => Err(format!("{v:?} is not a {what} value")),
    }
}

fn int_of(v: &Ipld, what: &str) -> R<i128> {
    match v {
        Ipld::Integer(i) => Ok(*i),
        _ => Err(format!("{v:?} is not a {what} value")),
    }
}

fn list_of<'a>(v: &'a Ipld, what: &str) -> R<&'a [Ipld]> {
    match v {
        Ipld::List(l) => Ok(l),
        _ => Err(format!("a {what} value is not a list")),
    }
}

fn encode(v: &Ipld) -> R<Vec<u8>> {
    serde_ipld_dagcbor::to_vec(v).map_err(|e| format!("a key cannot be encoded as DAG-CBOR: {e}"))
}

macro_rules! int_impl {
    ($($t:ty => $name:literal, $kind:expr);* $(;)?) => {$(
        impl Describe for $t {
            fn type_name() -> String {
                $name.into()
            }
            fn layout() -> Ipld {
                text($name)
            }
            fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
                Ok(Ipld::Integer(*self as i128))
            }
            fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
                let i = int_of(v, $name)?;
                <$t>::try_from(i).map_err(|_| format!("{i} does not fit {}", $name))
            }
            fn flecs() -> Flecs {
                Flecs::Primitive($kind)
            }
        }
    )*};
}

int_impl! {
    u8 => "u8", sys::EcsU8;
    u16 => "u16", sys::EcsU16;
    u32 => "u32", sys::EcsU32;
    u64 => "u64", sys::EcsU64;
    i8 => "i8", sys::EcsI8;
    i16 => "i16", sys::EcsI16;
    i32 => "i32", sys::EcsI32;
    i64 => "i64", sys::EcsI64;
    usize => "uptr", sys::EcsUPtr;
    isize => "iptr", sys::EcsIPtr;
}

/// `NonZero<T>` for the integer kinds: the logical type `NonZero<T>` over the physical integer (SPEC.md, "Logical
/// types"), value the bare integer. Unsigned: `{"logical": "NonZero<u32>", "of": "u32", "rules": {"range": [1,
/// null]}}`; signed (0 is a hole in the middle of the range): `{"validated": ["sync.checks.NonZero"]}`, the check every
/// world has. Restoring a zero is refused (a refusal, never a substitute value). Opaque to Flecs (shown as the integer,
/// not writable): a Flecs Script or REST write of a zero into one would be undefined behaviour in Rust.
macro_rules! nonzero_impl {
    ($($t:ty => $name:literal, $signed:literal);* $(;)?) => {$(
        impl Describe for core::num::NonZero<$t> {
            fn type_name() -> String {
                concat!("NonZero<", $name, ">").into()
            }
            fn layout() -> Ipld {
                nonzero_layout(Self::type_name(), $name, $signed)
            }
            fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
                Ok(Ipld::Integer(self.get() as i128))
            }
            fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
                let i = int_of(v, $name)?;
                let x = <$t>::try_from(i).map_err(|_| format!("{i} does not fit {}", $name))?;
                core::num::NonZero::new(x)
                    .ok_or_else(|| concat!("0 is not a NonZero<", $name, "> value").to_string())
            }
        }
    )*};
}

fn nonzero_layout(name: String, prim: &str, signed: bool) -> Ipld {
    let rule = if signed {
        (
            "validated",
            Ipld::List(vec![text(flecs_snapshot::logical::NONZERO_CHECK)]),
        )
    } else {
        ("range", Ipld::List(vec![Ipld::Integer(1), Ipld::Null]))
    };
    crate::__private::refine(text(prim), Some(name), vec![rule])
}

nonzero_impl! {
    u8 => "u8", false;
    u16 => "u16", false;
    u32 => "u32", false;
    u64 => "u64", false;
    i8 => "i8", true;
    i16 => "i16", true;
    i32 => "i32", true;
    i64 => "i64", true;
    usize => "uptr", false;
    isize => "iptr", true;
}

/// `u128` and `i128` as `{hi, lo}` halves (DAG-CBOR integers stop at 64 bits).
macro_rules! wide_impl {
    ($t:ty, $name:literal, $hi:ty) => {
        impl Describe for $t {
            fn type_name() -> String {
                $name.into()
            }
            fn layout() -> Ipld {
                struct_layout(
                    Some($name.into()),
                    vec![
                        ("hi".into(), <$hi>::layout(), None),
                        ("lo".into(), u64::layout(), None),
                    ],
                )
            }
            fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
                let hi = (*self >> 64) as $hi;
                let lo = *self as u64;
                Ok(struct_value(vec![
                    ("hi", Ipld::Integer(hi as i128)),
                    ("lo", Ipld::Integer(lo as i128)),
                ]))
            }
            fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
                let m = take_struct(v, &["hi", "lo"])?;
                let hi = <$hi>::from_value(get(m, "hi")?, ids).map_err(|e| member_err("hi", e))?;
                let lo = u64::from_value(get(m, "lo")?, ids).map_err(|e| member_err("lo", e))?;
                Ok(((hi as $t) << 64) | lo as $t)
            }
        }
    };
}

wide_impl!(u128, "u128", u64);
wide_impl!(i128, "i128", i64);

impl Describe for bool {
    fn type_name() -> String {
        "bool".into()
    }
    fn layout() -> Ipld {
        text("bool")
    }
    fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
        Ok(Ipld::Bool(*self))
    }
    fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
        match v {
            Ipld::Bool(b) => Ok(*b),
            _ => Err(format!("{v:?} is not a bool value")),
        }
    }
    fn flecs() -> Flecs {
        Flecs::Primitive(sys::EcsBool)
    }
}

/// A `char` is its code point, `"u32"`.
impl Describe for char {
    fn type_name() -> String {
        "char".into()
    }
    fn layout() -> Ipld {
        text("u32")
    }
    fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
        Ok(Ipld::Integer(*self as u32 as i128))
    }
    fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
        let i = int_of(v, "u32")?;
        u32::try_from(i)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| format!("{i} is not a Unicode scalar value"))
    }
    fn flecs() -> Flecs {
        Flecs::Primitive(sys::EcsU32)
    }
}

impl Describe for f32 {
    fn type_name() -> String {
        "f32".into()
    }
    fn layout() -> Ipld {
        text("f32")
    }
    fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
        float(*self as f64)
    }
    fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
        let f = float_of(v, "f32")?;
        let x = f as f32;
        if x as f64 == f {
            Ok(x)
        } else {
            Err(format!("{f} is not exactly an f32"))
        }
    }
    fn flecs() -> Flecs {
        Flecs::Primitive(sys::EcsF32)
    }
}

impl Describe for f64 {
    fn type_name() -> String {
        "f64".into()
    }
    fn layout() -> Ipld {
        text("f64")
    }
    fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
        float(*self)
    }
    fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
        float_of(v, "f64")
    }
    fn flecs() -> Flecs {
        Flecs::Primitive(sys::EcsF64)
    }
}

impl Describe for String {
    fn type_name() -> String {
        "String".into()
    }
    fn layout() -> Ipld {
        text("string")
    }
    fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
        Ok(Ipld::String(self.clone()))
    }
    fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
        match v {
            Ipld::String(s) => Ok(s.clone()),
            Ipld::Null => Err("a Rust String cannot be null".into()),
            _ => Err(format!("{v:?} is not a string value")),
        }
    }
}

impl Describe for PathBuf {
    fn type_name() -> String {
        "PathBuf".into()
    }
    fn layout() -> Ipld {
        text("string")
    }
    fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
        self.to_str()
            .map(|s| Ipld::String(s.to_string()))
            .ok_or_else(|| format!("the path {self:?} is not UTF-8"))
    }
    fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
        String::from_value(v, ids).map(PathBuf::from)
    }
}

/// A unit type: a struct with no members.
macro_rules! empty_struct {
    ($name:literal) => {
        fn type_name() -> String {
            $name.into()
        }
        fn layout() -> Ipld {
            struct_layout(None, vec![])
        }
        fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
            Ok(struct_value(vec![]))
        }
        fn flecs() -> Flecs {
            Flecs::Struct(FlecsStruct {
                name: $name.into(),
                size: 0,
                align: 1,
                members: vec![],
            })
        }
    };
}

impl Describe for () {
    empty_struct!("()");
    fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
        take_struct(v, &[]).map(|_| ())
    }
}

impl<T: ?Sized + 'static> Describe for PhantomData<T> {
    empty_struct!("PhantomData");
    fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
        take_struct(v, &[]).map(|_| PhantomData)
    }
}

/// `Option<T>`: `{"option": T}`, `[]` for none and `[v]` for some.
impl<T: Describe> Describe for Option<T> {
    fn type_name() -> String {
        format!("Option<{}>", T::type_name())
    }
    fn layout() -> Ipld {
        single("option", T::layout())
    }
    fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
        Ok(Ipld::List(match self {
            None => vec![],
            Some(x) => vec![x.read(refs).map_err(|e| member_err("[0]", e))?],
        }))
    }
    fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
        match list_of(v, "option")? {
            [] => Ok(None),
            [x] => T::from_value(x, ids)
                .map(Some)
                .map_err(|e| member_err("[0]", e)),
            l => Err(format!("an option value has {} elements (0 or 1)", l.len())),
        }
    }
}

/// `Result<T, E>`: a union of `Ok` and `Err`, each a struct with member `0`.
impl<T: Describe, E: Describe> Describe for Result<T, E> {
    fn type_name() -> String {
        format!("Result<{}, {}>", T::type_name(), E::type_name())
    }
    fn layout() -> Ipld {
        let payload = |l| struct_layout(None, vec![("0".into(), l, None)]);
        union_layout(
            Self::type_name(),
            vec![
                ("Ok".into(), Some(payload(T::layout()))),
                ("Err".into(), Some(payload(E::layout()))),
            ],
        )
    }
    fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
        let (n, v) = match self {
            Ok(x) => ("Ok", x.read(refs)),
            Err(x) => ("Err", x.read(refs)),
        };
        let v = v.map_err(|e| member_err(&format!("{n}.0"), e))?;
        Ok(union_value(n, Some(struct_value(vec![("0", v)]))))
    }
    fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
        let (n, p) = take_union(v)?;
        let m = take_struct(p, &["0"]).map_err(|e| member_err(n, e))?;
        let x = get(m, "0")?;
        match n {
            "Ok" => T::from_value(x, ids)
                .map(Ok)
                .map_err(|e| member_err("Ok.0", e)),
            "Err" => E::from_value(x, ids)
                .map(Err)
                .map_err(|e| member_err("Err.0", e)),
            _ => Err(crate::__private::unknown_variant(n, "Result")),
        }
    }
}

macro_rules! vector_impl {
    ($t:ident, $name:literal) => {
        impl<T: Describe> Describe for $t<T> {
            fn type_name() -> String {
                format!(concat!($name, "<{}>"), T::type_name())
            }
            fn layout() -> Ipld {
                single("vector", T::layout())
            }
            fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
                self.iter()
                    .enumerate()
                    .map(|(i, x)| x.read(refs).map_err(|e| member_err(&format!("[{i}]"), e)))
                    .collect::<R<_>>()
                    .map(Ipld::List)
            }
            fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
                list_of(v, "vector")?
                    .iter()
                    .enumerate()
                    .map(|(i, x)| {
                        T::from_value(x, ids).map_err(|e| member_err(&format!("[{i}]"), e))
                    })
                    .collect()
            }
        }
    };
}

vector_impl!(Vec, "Vec");
vector_impl!(VecDeque, "VecDeque");

/// Sets: a vector sorted by the elements' DAG-CBOR, with no duplicate.
fn set_read<'a, T: Describe + 'a>(
    items: impl Iterator<Item = &'a T>,
    refs: &mut dyn Refs,
) -> R<Ipld> {
    let mut v: Vec<(Vec<u8>, Ipld)> = items
        .map(|x| {
            let v = x.read(refs)?;
            Ok((encode(&v)?, v))
        })
        .collect::<R<_>>()?;
    v.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Ipld::List(v.into_iter().map(|(_, v)| v).collect()))
}

macro_rules! set_impl {
    ($t:ident, $name:literal, [$($b:tt)*] $(, $s:ident)?) => {
        impl<T: Describe + $($b)* $(, $s: BuildHasher + Default + 'static)?> Describe for $t<T $(, $s)?> {
            fn type_name() -> String {
                format!(concat!($name, "<{}>"), T::type_name())
            }
            fn layout() -> Ipld {
                single("vector", T::layout())
            }
            fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
                set_read(self.iter(), refs)
            }
            fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
                let mut out = Self::default();
                for (i, x) in list_of(v, "set")?.iter().enumerate() {
                    let x = T::from_value(x, ids).map_err(|e| member_err(&format!("[{i}]"), e))?;
                    if !out.insert(x) {
                        return Err(format!("element {i} of a set value is a duplicate"));
                    }
                }
                Ok(out)
            }
        }
    };
}

set_impl!(BTreeSet, "BTreeSet", [Ord]);
set_impl!(HashSet, "HashSet", [Eq + Hash], S);

/// Maps: `{"map": [K, V]}`, an array of `[key, value]` sorted by the key's DAG-CBOR, keys unique.
fn map_layout<K: Describe, V: Describe>() -> Ipld {
    single("map", Ipld::List(vec![K::layout(), V::layout()]))
}

fn map_read<'a, K: Describe + 'a, V: Describe + 'a>(
    entries: impl Iterator<Item = (&'a K, &'a V)>,
    refs: &mut dyn Refs,
) -> R<Ipld> {
    let mut v: Vec<(Vec<u8>, Ipld)> = entries
        .enumerate()
        .map(|(i, (k, v))| {
            let k = k
                .read(refs)
                .map_err(|e| member_err(&format!("[{i}].key"), e))?;
            let v = v
                .read(refs)
                .map_err(|e| member_err(&format!("[{i}].value"), e))?;
            Ok((encode(&k)?, Ipld::List(vec![k, v])))
        })
        .collect::<R<_>>()?;
    v.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Ipld::List(v.into_iter().map(|(_, e)| e).collect()))
}

fn map_entries(v: &Ipld) -> R<Vec<(&Ipld, &Ipld)>> {
    list_of(v, "map")?
        .iter()
        .map(|e| match e {
            Ipld::List(kv) if kv.len() == 2 => Ok((&kv[0], &kv[1])),
            _ => Err("a map entry is not [key, value]".to_string()),
        })
        .collect()
}

macro_rules! map_impl {
    ($t:ident, $name:literal, [$($b:tt)*] $(, $s:ident)?) => {
        impl<K: Describe + $($b)*, V: Describe $(, $s: BuildHasher + Default + 'static)?> Describe for $t<K, V $(, $s)?> {
            fn type_name() -> String {
                format!(concat!($name, "<{}, {}>"), K::type_name(), V::type_name())
            }
            fn layout() -> Ipld {
                map_layout::<K, V>()
            }
            fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
                map_read(self.iter(), refs)
            }
            fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
                let mut out = Self::default();
                for (i, (k, x)) in map_entries(v)?.into_iter().enumerate() {
                    let k = K::from_value(k, ids).map_err(|e| member_err(&format!("[{i}].key"), e))?;
                    let x = V::from_value(x, ids).map_err(|e| member_err(&format!("[{i}].value"), e))?;
                    if out.insert(k, x).is_some() {
                        return Err(format!("key {i} of a map value is a duplicate"));
                    }
                }
                Ok(out)
            }
        }
    };
}

map_impl!(BTreeMap, "BTreeMap", [Ord]);
map_impl!(HashMap, "HashMap", [Eq + Hash], S);

/// `[T; N]`: `{"array": T, "count": N}`.
impl<T: Describe, const N: usize> Describe for [T; N] {
    fn type_name() -> String {
        format!("[{}; {N}]", T::type_name())
    }
    fn layout() -> Ipld {
        let mut m = BTreeMap::new();
        m.insert("array".to_string(), T::layout());
        m.insert("count".to_string(), Ipld::Integer(N as i128));
        Ipld::Map(m)
    }
    fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
        self.iter()
            .enumerate()
            .map(|(i, x)| x.read(refs).map_err(|e| member_err(&format!("[{i}]"), e)))
            .collect::<R<_>>()
            .map(Ipld::List)
    }
    fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
        let l = list_of(v, "array")?;
        if l.len() != N {
            return Err(format!(
                "the array value has {} elements where {N} are expected",
                l.len()
            ));
        }
        let items: Vec<T> = l
            .iter()
            .enumerate()
            .map(|(i, x)| T::from_value(x, ids).map_err(|e| member_err(&format!("[{i}]"), e)))
            .collect::<R<_>>()?;
        items
            .try_into()
            .map_err(|_| "array length changed while converting".to_string())
    }
    fn flecs() -> Flecs {
        Flecs::Array {
            name: Self::type_name(),
            elem: Box::new(T::flecs()),
            count: N,
            size: core::mem::size_of::<Self>(),
            align: core::mem::align_of::<Self>(),
        }
    }
}

/// Smart pointers are described as what they hold; restoring makes a new one.
macro_rules! pointer_impl {
    ($t:ident) => {
        impl<T: Describe> Describe for $t<T> {
            fn type_name() -> String {
                T::type_name()
            }
            fn layout() -> Ipld {
                T::layout()
            }
            fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
                (**self).read(refs)
            }
            fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
                T::from_value(v, ids).map($t::new)
            }
        }
    };
}

pointer_impl!(Box);
pointer_impl!(Arc);
pointer_impl!(Rc);

/// Tuples: structs with no name whose members are `0`, `1`, ...
macro_rules! tuple_impl {
    ($($t:ident $i:tt),+) => {
        impl<$($t: Describe),+> Describe for ($($t,)+) {
            fn type_name() -> String {
                let names: Vec<String> = vec![$($t::type_name()),+];
                format!("({})", names.join(", "))
            }
            fn layout() -> Ipld {
                struct_layout(None, vec![$((stringify!($i).into(), $t::layout(), None)),+])
            }
            fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
                Ok(struct_value(vec![$((
                    stringify!($i),
                    self.$i.read(refs).map_err(|e| member_err(stringify!($i), e))?,
                )),+]))
            }
            fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
                let m = take_struct(v, &[$(stringify!($i)),+])?;
                Ok(($(
                    $t::from_value(get(m, stringify!($i))?, ids).map_err(|e| member_err(stringify!($i), e))?,
                )+))
            }
            fn flecs() -> Flecs {
                Flecs::Struct(FlecsStruct {
                    name: Self::type_name(),
                    size: core::mem::size_of::<Self>(),
                    align: core::mem::align_of::<Self>(),
                    members: vec![$(FlecsMember {
                        name: stringify!($i).into(),
                        offset: core::mem::offset_of!(Self, $i),
                        size: core::mem::size_of::<$t>(),
                        ty: $t::flecs(),
                    }),+],
                })
            }
        }
    };
}

tuple_impl!(A 0);
tuple_impl!(A 0, B 1);
tuple_impl!(A 0, B 1, C 2);
tuple_impl!(A 0, B 1, C 2, D 3);
tuple_impl!(A 0, B 1, C 2, D 3, E 4);
tuple_impl!(A 0, B 1, C 2, D 3, E 4, F 5);
tuple_impl!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
tuple_impl!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
tuple_impl!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8);
tuple_impl!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9);
tuple_impl!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10);
tuple_impl!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11);
