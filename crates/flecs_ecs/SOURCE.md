# flecs_ecs 0.2.2, maintained here on upstream Flecs master

- Upstream: https://github.com/Indra-db/Flecs-Rust, rev `bf956db99b69d41ee142c9d52054911af28d7e93` (its head on
  2026-10-29; upstream stopped at Flecs 4.1.6), directory `flecs_ecs/`. Taken on 2026-10-29 from the space game's
  `vendor/flecs_ecs` (`src/`, `LICENSE`, `README.md`), which is upstream plus the wasm32 fixes below. Upstream's
  `tests/`, `benches/`, `examples/` and `assets/` are not copied.
- Licence: MIT (`LICENSE`, Indra de Backere). Flecs C is MIT (Sander Mertens), `../../vendor/flecs/LICENSE`.
- `Cargo.toml` is upstream's with the workspace keys resolved (edition 2024, licence MIT, repository, rust-version
  1.97), `flecs_ecs_derive` and `flecs_ecs_sys` as the sibling crates, `[lints]`, dev-dependencies, `[[example]]`,
  `[[bench]]` and the `test-with-crash-handler` feature dropped, `publish = false`, no doctests (upstream's doc
  examples are not kept compiling here), and `autotests`/`autobenches`/`autoexamples = false`.
- Fixes worth upstreaming go to Flecs-Rust as PRs; this copy moves with `vendor/flecs` (`scripts/bump_flecs.py`).

## Changes from upstream

wasm32 (from the space game's copy, 2026-09-26; each marked `rgb:` in the source):

- `src/addons/meta/meta_fn_types.rs`: `use crate::core::{Entity, WorldRef}` for every target.
- `src/addons/app/app.rs`: `frame_action_trampoline` and `run_action_trampoline` take their ABI from `#[extern_abi]`.
- `src/core/query_iter.rs`: the `make_iter` field is `extern "C"` on wasm, matching the bindings.

Flecs master (`c73ce7115`, 2026-10-29): see "Port to Flecs master" below, and `../flecs_ecs_sys/SOURCE.md` for the C
side.

- `flecs_safety_locks` is no longer in `flecs_base` (so not a default): it needs Flecs-Rust's patch to Flecs C
  (`FLECS_MUT_ALIAS_LOCKS`), which upstream does not have, and enabling it is a build error.
- `flecs_script_shell`: Flecs's script shell addon.
