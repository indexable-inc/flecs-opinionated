//! Run Flecs scripts in a world and read it back as DAG-CBOR: one document, or a Merkle DAG of entity blocks under one
//! root CID. The format is in SPEC.md. The same code builds natively (rlib, cdylib, staticlib with `include/world.h`)
//! and for wasm32-unknown-unknown with no imports. The C ABI is behind the `ffi` feature.

mod cbor;
pub mod check;
pub mod dag;
pub mod described;
#[cfg(feature = "ffi")]
mod ffi;
pub mod host;
pub mod merge;
pub mod migrate;
pub mod mode;
mod mst;
mod schema;
mod walk;
pub mod world;

pub use dag::{
    BlockStore, Blocks, Change, Countersignature, DAG_CBOR, Difference, Kind, MemoryStore, RAW,
    VERSION, cid_of, cid_with, diff, read_blob,
};
pub use described::{Described, ReadFn, Refs, Resolve, WriteFn, describe, described};
pub use host::{Encode, Host, Ids, Outside, Restore, Restored, Selection};
pub use merge::{Rule, merge_fields, merge_state};
pub use migrate::{Migrations, Report};
pub use mode::Mode;
pub use world::World;

#[cfg(test)]
mod tests;
