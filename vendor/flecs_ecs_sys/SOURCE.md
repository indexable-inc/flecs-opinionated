# flecs_ecs_sys 0.2.1, as the space game vendors it

- Upstream: https://github.com/Indra-db/Flecs-Rust, rev `bf956db99b69d41ee142c9d52054911af28d7e93`, directory
  `flecs_ecs_sys/`. Copied on 2026-10-25 from the space game's `vendor/flecs_ecs_sys/` (its SOURCE.md has the full
  history), which is upstream plus the Flecs C fork patches that `../flecs/SOURCE.md` describes.
- Changes from the game's copy:
  - `src/flecs.c` and `src/flecs.h` are not copied. `build.rs` adds `../flecs/distr` to the include path, so
    `src/flecs_rust.c`'s `#include "flecs.c"` compiles this repository's pinned amalgamation, the same bytes
    `crates/flecs-c` compiles (`cmp` against the game's `src/flecs.c` was equal on 2026-10-25).
  - The game's `rgb-build-flags` build-dependency and its one call in `build.rs` are dropped.
- Used only by `host-check/` (a separate workspace), through `[patch."https://github.com/Indra-db/Flecs-Rust"]`, as
  the game wires its copy in. Nothing in the main workspace builds it.
- Licences: MIT. `LICENSE` is flecs_ecs_sys's (Indra de Backere); `LICENSE-flecs` is Flecs C's (Sander Mertens).
