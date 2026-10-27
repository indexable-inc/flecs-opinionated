# Flecs C, upstream master

`distr/flecs.c` and `distr/flecs.h` are the amalgamation from Flecs C master `72a531d6f706c19629f15f3d49cb12a8d5dbe5e1`
("[script] Use Parent hierarchy by default for prefab and template scopes", 2026-10-27; 645 commits past `6e4cd338a`,
which is what 4.1.6 shipped as; `FLECS_VERSION_*` still says 4.1.6), copied from upstream's `distr/` with the local
changes in `local.patch` (`git apply -p1 --directory=vendor/flecs local.patch` onto upstream's two files), each marked
`local:` in the source:

- wasm: a plain `__wasm__` target (wasm32-unknown-unknown, `crates/flecs-c/wasm-libc`) sets `ECS_TARGET_WASM` and
  `ECS_TARGET_POSIX`, and includes no `execinfo.h`. Upstream knows only Emscripten.
- string buffer growth checked (`flecs_strbuf_grow`): the `int32_t` size no longer wraps past 1 GiB into a NULL buffer
  that later asserts `b->content != NULL`; it grows to at most `INT32_MAX` and aborts naming the cause
  (`ECS_OUT_OF_MEMORY`) when it cannot grow or the allocation fails. Upstream still doubles an `int32_t`.

Dropped from the old pin (the space game's fork of 4.1.6):

- `rgb-fork.patch`, upstream `ed776dd` (#2219, no opaque callbacks under a meta cursor with no data): upstream has it.
- exact JSON floats and `rgb-rest-gate.patch` (REST writes gated): the JSON and REST addons are not compiled here
  (`crates/flecs-c/build.rs`), so the hunks changed nothing this library runs.
- the Flecs-Rust `FLECS_MUT_ALIAS_LOCKS` hunks the game's copy carries: this library never defines it.

To move the pin: copy upstream's `distr/flecs.c` and `distr/flecs.h`, apply `local.patch` (refresh it if a hunk
moved), update the commit above, and run `uv run build.py --test`.

The space game's pin is separate and behind: Flecs C `6e4cd338a` (4.1.6) plus its fork, in the game's
`vendor/flecs_ecs_sys/src/`. A copy of those two files is `../flecs_ecs_sys/src/flecs.{c,h}`, which host-check builds
so that host mode (the `host` feature of `flecs-c`, SPEC.md "Host worlds") is still checked against the game's Flecs:
in host mode `crates/flecs-c` compiles `csrc/glue.c` against the host's `flecs.h`, not this one. Nothing requires the
two pins to be equal any more.

Licence: MIT (Sander Mertens), `LICENSE`.
