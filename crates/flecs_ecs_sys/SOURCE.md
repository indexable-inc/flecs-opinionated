# flecs_ecs_sys 0.2.1, maintained here on upstream Flecs master

- Upstream: https://github.com/Indra-db/Flecs-Rust, rev `bf956db99b69d41ee142c9d52054911af28d7e93`, directory
  `flecs_ecs_sys/`. Taken on 2026-10-29 from the space game's `vendor/flecs_ecs_sys` (`src/` without `flecs.c` and
  `flecs.h`, `build.rs`, `Cargo.toml`, `LICENSE`, `LICENSE-flecs`).
- Licences: MIT. `LICENSE` is flecs_ecs_sys's (Indra de Backere); `LICENSE-flecs` is Flecs C's (Sander Mertens).

## Flecs C

There is no Flecs C in this crate. `src/flecs_rust.c` includes `flecs.c` from the repository's one amalgamation,
`../../vendor/flecs/distr` (upstream master, `../../vendor/flecs/SOURCE.md`; `FLECS_DISTR` names another directory),
so every process that links this crate has exactly that Flecs. `crates/flecs-c` with `host` compiles only its glue
against the same `flecs.h` and links this crate's Flecs. A graph with both this crate and `flecs-c` without `host`
has two Flecs and fails to link (duplicate `ecs_*` symbols): turn on `host` (`flecs-describe`'s `flecs_ecs` feature
does).

Upstream Flecs-Rust's `flecs.c`/`flecs.h` carried its own patch (`FLECS_MUT_ALIAS_LOCKS`: per-column and
per-sparse-record read/write locks, `ecs_get_ptr_t` with a lock target, `flecs_record_get_id`,
`flecs_record_get_mut_id`, an `other_table` for `on_replace`). Upstream Flecs has none of it, and this repository does
not patch Flecs C, so:

- `ecs_get_ptr_t` is always `void *` (`src/flecs_rust.h`), and `flecs_record_get_id` / `flecs_record_get_mut_id` are
  defined in `src/flecs_rust.c` as `ecs_get_id` / `ecs_get_mut_id` with the caller's record;
- the `flecs_safety_locks` feature is a build error and no longer in `flecs_base`;
- an `on_replace` hook gets no `other_table`.

## Changes from upstream

- `build.rs`:
  - compiles `src/flecs_rust.c` with `vendor/flecs/distr` on the include path; generates bindings against it;
  - always defines the addons upstream master split out of the core and flecs_ecs uses (`ALWAYS`: CACHED_QUERIES,
    QUERY_PLANS, QUERY_DSL, PARSER, PREFAB, CONSTRAINT_TRAITS, ENTITY_RANGES, MULTI_WORLD). Some size structs the
    bindings describe (`ecs_query_iter_t`), so they are on whatever the features;
  - `flecs_script_shell` defines `FLECS_SCRIPT_SHELL`;
  - wasm32 (from the space game, marked `rgb:`): `FLECS_NO_OS_API_IMPL`, no HTTP or REST, wasi-libc from
    `FLECS_WASI_SYSROOT` (or the game's `RGB_WASI_SYSROOT`), and the unwind flags skipped by the target's family;
  - the game's `rgb_build_flags::apply` call and build-dependency are gone: an application's optimised C flags
    (`-mcpu`, `-fvisibility=hidden`) go in cc's `CFLAGS` / `CFLAGS_<target>`.
- `src/flecs_rust.c`, `src/flecs_rust.h`: ported to master's internals (commands instead of the removed defer API,
  `flecs_defer_cmd(stage, deferred)`, the queued-remove check of `flecs_defer_set_impl`, `flecs_ensure`'s derived
  component check, no previous table for `flecs_invoke_replace_hook`), and the record getters above.
- `src/bindings.rs`, `src/bindings_release.rs`: generated from master's `flecs.h` (`scripts/bump_flecs.py`), with the
  space game's wasm `FILE` lines (`rgb:`) kept at the top.
- `src/lib.rs` (from the game, `rgb:`): on wasm the bindings module includes `OUT_DIR/bindings_wasm.rs`, the profile's
  bindings with `extern "C-unwind"` made `extern "C"`.
- `Cargo.toml`: upstream's with the workspace keys resolved, `[lints]` dropped, `publish = false`,
  `autotests = false` (upstream's `tests/check_for_changes.rs` is not copied).
