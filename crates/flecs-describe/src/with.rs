//! Descriptions for a field whose type has no [`Describe`](crate::Describe) of its own, or should be described
//! differently: `#[describe(with = flecs_describe::with::bytes)]`.
//!
//! A `with` path names a module (or any path) with three functions, which the derive calls for the field:
//!
//! ```ignore
//! fn layout() -> Ipld;
//! fn read(x: &T, refs: &mut dyn Refs) -> R<Ipld>;
//! fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<T>;
//! ```
//!
//! A field described this way is opaque to Flecs's reflection (its type gives no [`Flecs`](crate::Flecs) shape).

/// `Vec<u8>` as `"bytes"`, a byte string inline (SPEC.md, "Values", `sync.Bytes`), rather than a vector of `u8`.
pub mod bytes {
    use crate::{Ipld, R, Refs, Resolve};

    pub fn layout() -> Ipld {
        Ipld::String("bytes".into())
    }

    pub fn read(x: &[u8], _: &mut dyn Refs) -> R<Ipld> {
        Ok(Ipld::Bytes(x.to_vec()))
    }

    pub fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Vec<u8>> {
        match v {
            Ipld::Bytes(b) => Ok(b.clone()),
            _ => Err("a bytes value is not a byte string".into()),
        }
    }
}

/// A `Display` and `FromStr` type as a `"string"` (a `Uuid`, a `SocketAddr`, a semantic version): the value is its
/// `to_string()`, restored by `parse`. Use as `#[describe(with = flecs_describe::with::display)]`.
pub mod display {
    use crate::{Ipld, R, Refs, Resolve};

    pub fn layout() -> Ipld {
        Ipld::String("string".into())
    }

    pub fn read<T: core::fmt::Display>(x: &T, _: &mut dyn Refs) -> R<Ipld> {
        Ok(Ipld::String(x.to_string()))
    }

    pub fn from_value<T>(v: &Ipld, _: &mut dyn Resolve) -> R<T>
    where
        T: core::str::FromStr,
        T::Err: core::fmt::Display,
    {
        match v {
            Ipld::String(s) => s.parse().map_err(|e| format!("{s:?} does not parse: {e}")),
            _ => Err("a string value is not text".into()),
        }
    }
}
