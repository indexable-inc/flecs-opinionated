//! Raw FFI bindings to the [`flecs`] library.
//!
//! These are only intended for use by the higher level bindings that build atop these.
//!
//! [`flecs`]: https://www.flecs.dev/

#[cfg(all(feature = "force_build_release", feature = "force_build_debug"))]
compile_error!(
    "Features 'force_build_release' and 'force_build_debug' cannot be enabled at the same time."
);

//skips whole file from cargo fmt --all --check for CI
//the two variants match the compiled C profile: several structs carry
//FLECS_DEBUG-gated fields, so debug and release C builds have different layouts
#[cfg(all(not(flecs_c_release), not(target_family = "wasm")))]
#[rustfmt::skip]
mod bindings;
#[cfg(all(flecs_c_release, not(target_family = "wasm")))]
#[rustfmt::skip]
#[path = "bindings_release.rs"]
mod bindings;
// rgb: on wasm, flecs_ecs's callbacks are plain `extern "C"` (no
// unwinding), so build.rs writes the profile's bindings with that ABI.
#[cfg(target_family = "wasm")]
#[rustfmt::skip]
#[allow(clippy::all, warnings)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/bindings_wasm.rs"));
}
mod extensions;
mod mbindings;

pub use bindings::*;
pub use mbindings::*;
