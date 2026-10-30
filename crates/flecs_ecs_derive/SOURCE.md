# flecs_ecs_derive 0.2.0, maintained here with flecs_ecs

- Upstream: https://github.com/Indra-db/Flecs-Rust, rev `bf956db99b69d41ee142c9d52054911af28d7e93`, directory
  `flecs_ecs_derive/` (`src/`, `LICENSE`), copied from the cargo git checkout on 2026-10-29. Brought in with
  `flecs_ecs` because the code it generates names `flecs_ecs` items that the Flecs master port changes.
- Licence: MIT (`LICENSE`, Indra de Backere).
- `Cargo.toml`: upstream's with the workspace keys resolved, `[lints]` and the dev-dependency on flecs_ecs dropped,
  `publish = false`. Upstream's `tests/` is not copied.

## Changes from upstream

- `#[flecs(traits((OnInstantiate, Copy)))]`: Flecs master renamed `(OnInstantiate, Override)` to
  `(OnInstantiate, Copy)`. `Override` is still accepted and means `Copy`.
- `SlotOf` is no longer an accepted trait name (Flecs master removed it).
