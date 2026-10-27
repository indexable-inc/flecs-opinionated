# flecs_ecs_sys 0.2.1, as the space game vendors it

- Upstream: https://github.com/Indra-db/Flecs-Rust, rev `bf956db99b69d41ee142c9d52054911af28d7e93`, directory
  `flecs_ecs_sys/`. Copied on 2026-10-25 from the space game's `vendor/flecs_ecs_sys/` (its SOURCE.md has the full
  history), which is upstream plus the game's Flecs C fork patches.
- Changes from the game's copy:
  - `src/flecs.c` and `src/flecs.h` are the game's amalgamation (Flecs C `6e4cd338a`, 4.1.6, plus its fork), copied
    from this repository's old `vendor/flecs/distr` on 2026-10-27 when that moved to upstream master
    (`../flecs/SOURCE.md`). They are the game's pin, kept so host-check builds host mode against the Flecs the game
    links; `crates/flecs-c` in host mode compiles its glue against this `flecs.h`.
  - The game's `rgb-build-flags` build-dependency and its one call in `build.rs` are dropped.
- Used only by `host-check/` (a separate workspace), through `[patch."https://github.com/Indra-db/Flecs-Rust"]`, as
  the game wires its copy in. Nothing in the main workspace builds it.
- Licences: MIT. `LICENSE` is flecs_ecs_sys's (Indra de Backere); `LICENSE-flecs` is Flecs C's (Sander Mertens).
