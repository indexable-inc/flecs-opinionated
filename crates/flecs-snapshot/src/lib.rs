//! Run Flecs scripts in a world and read it back as DAG-CBOR: one document, or a Merkle DAG of entity blocks under one
//! root CID. The format is in SPEC.md. The same code builds natively (rlib, cdylib, staticlib with `include/world.h`)
//! and for wasm32-unknown-unknown with no imports.

pub mod dag;
mod ffi;
pub mod merge;
mod schema;
mod walk;
pub mod world;

pub use dag::{
    BlockStore, Blocks, Change, Countersignature, Difference, Kind, MemoryStore, VERSION, cid_of,
    diff,
};
pub use merge::{Merger, Rule};
pub use world::World;

#[cfg(test)]
mod tests;
