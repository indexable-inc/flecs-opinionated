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

## Port to Flecs master

Master has no implicit defer mode: an operation is queued only when it is issued through a stage (`ecs_is_stage`),
and `ecs_merge(stage)` flushes one stage. The iterator and query fields were renamed (the old `world`, a stage, is
`stage`; the old `real_world` is `world`), and the builtin ids were renumbered (`c_types.rs` follows master). Upstream
Flecs's C++ API (`include/flecs/addons/cpp/`) made the same moves; this port follows it.

API changes for users:

- `World::defer_begin`, `defer_end`, `defer_suspend`, `defer_resume`, `is_defer_suspended` are gone. `World::defer`
  now passes the stage: `world.defer(|stage| { ... })`, and operations go through `stage`. On a stage handle it runs
  the closure directly; otherwise it uses stage 0 and merges it after the closure (not while readonly).
- `World::is_deferred` is `ecs_is_stage` (true for a stage handle).
- `World::with`, `with_enum`, `with_enum_pair` and `EntityView::with`, `with_first`, `with_second` are gone (master
  removed `ecs_set_with`/`ecs_get_with`): add the ids explicitly.
- `EntityView::slot_of`, `slot`, `flecs::SlotOf` and `flecs::DefaultChildComponent` are gone.
- `flecs::Copy` is new (master renamed `(OnInstantiate, Override)`); `flecs::Override` is an alias of it, and
  `OnInstantiatePolicy::Override` is `OnInstantiatePolicy::Copy` (the default).
- `TableIter::to_str` is gone (master removed `ecs_iter_str`).
- `get` callbacks, singleton callbacks and the table guards (`TableEntities`, `TableColumnMut`) lock the table instead
  of deferring (`ecs_table_lock`, as master's C++ does): a structural change inside them asserts in debug builds and
  is not queued.
- `TableFlags::HasDontFragment` is `EdgeDontFragment`; `HasDerived`, `HasParent`, `Empty`, `NotEmpty`, `HasUpNotify`
  and `RemoveActionFlags` are new.
- New: `EntityView::ordered_children` (`ecs_get_ordered_children`, for `(OrderedChildren)` and `Parent` children),
  `ScriptEntityView::reload(Option<&str>)` and `undo()`, `ScriptBuilder::skip_unknown` and `ir`, and with
  `flecs_script_shell` `World::command_session` and `World::command_run` (`src/addons/script/shell.rs`).
- `sys::ecs_entity_desc_t` has no `add`, `add_expr` or `set`; master removed `ecs_record_find`, so
  record lookups use `sys::ecs_rust_record_find` (`flecs_ecs_sys/src/flecs_rust.c`, 4.1.6's semantics).
- `System::update` and `Observer::update` are unchanged.

`flecs::Parent`, `flecs::OrderedChildren`, `flecs::DontFragment` and `flecs::Sparse` were already exposed.
