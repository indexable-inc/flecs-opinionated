# Flecs C, the game's pin

`distr/flecs.c` and `distr/flecs.h` are byte for byte the amalgamation the space game links
(`the-game/vendor/flecs_ecs_sys/src/`, flecs_ecs_sys 0.2.1 at Flecs-Rust `bf956db`): Flecs C master
`6e4cd338a` (4.1.6) plus the game's fork changes, each marked `rgb fork` or `rgb:` in the source:

- `rgb-fork.patch`: upstream `ed776dd` (#2219), no opaque callbacks under a meta cursor with no data;
- exact JSON floats (`flecs_json_ser_float`; JSON is not compiled here);
- `rgb-rest-gate.patch`: REST writes gated (REST is not compiled here);
- wasm: a plain `__wasm__` target sets `ECS_TARGET_WASM` and `ECS_TARGET_POSIX`, and no `execinfo.h` there.

One pin for both repositories, so a host that links its own Flecs (the `host` feature of `flecs-c`, SPEC.md "Host
worlds") runs this library's glue against the same headers and the same code it was tested with. When the game moves
its pin, copy its two files here in the same change, and the other way round.

Licence: MIT (Sander Mertens), `LICENSE`.
