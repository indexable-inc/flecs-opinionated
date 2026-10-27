# Notes for agents

- Prefer runtime data over Rust generics for anything a host, TS or script must see (merge rules, layouts, schema).
  Generics are compile-time only; if a typed Rust API is wanted, it is a thin wrapper over the runtime, data-driven
  one.
- No project name in code: C symbols, headers, TS identifiers, wasm exports and file names use neutral names
  (`world_new`, `world.h`, `Flecs`, `World`). The name appears only in manifests, the README title and CI.
- Flecs is linked in one place, `crates/flecs-c` (cc, the same sources for native and wasm32-unknown-unknown, no
  WASI). Everything above Flecs is Rust in `crates/flecs-snapshot`.
- The format is SPEC.md. A change to what the library writes is a spec change first: additive within a version,
  otherwise a new version. Layout hashes cover anything that changes how a value is read or merged.
- `merge/` and `lean/` belong to another lane; call them through `flecs_snapshot::merge` (`merge_state`,
  `merge_fields`), which converts the DAG-CBOR state encoding (SPEC "Merge states").
- Build and test with `uv run build.py --test` in an ix VM, not a laptop.
- Flecs C is upstream, unpatched. `vendor/flecs/distr` is the amalgamation of one upstream SanderMertens/flecs
  commit, named in `vendor/flecs/SOURCE.md`. A Flecs bug or missing feature is fixed with a PR to SanderMertens/flecs
  (from a fork under andrewgazelka), not a local patch. Until it merges, pin the PR's fork branch commit in SOURCE.md
  with a link to the PR, and move back to an upstream master commit once it lands. No "rgb fork" hunks.
