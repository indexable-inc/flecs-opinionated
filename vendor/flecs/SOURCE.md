# Flecs C, the game's pin

`distr/flecs.c` and `distr/flecs.h` are byte for byte the amalgamation the space game links
(`the-game/vendor/flecs_ecs_sys/src/`, flecs_ecs_sys 0.2.1 at Flecs-Rust `bf956db`): Flecs C master
`6e4cd338a` (4.1.6) plus the game's fork changes, each marked `rgb fork` or `rgb:` in the source:

- `rgb-fork.patch`: upstream `ed776dd` (#2219), no opaque callbacks under a meta cursor with no data;
- exact JSON floats (`flecs_json_ser_float`; JSON is not compiled here);
- `rgb-rest-gate.patch`: REST writes gated (REST is not compiled here);
- string buffer growth checked (`flecs_strbuf_grow`): the `int32_t` size no longer wraps past 1 GiB into a NULL
  buffer that later asserts `b->content != NULL`; it grows to at most `INT32_MAX` and aborts naming the cause
  (`ECS_OUT_OF_MEMORY`) when it cannot grow or the allocation fails. Added 2026-10-26 upstream first: the game's
  `vendor/flecs_ecs_sys/src/flecs.c` needs the same hunk to stay byte for byte equal;
- wasm: a plain `__wasm__` target sets `ECS_TARGET_WASM` and `ECS_TARGET_POSIX`, and no `execinfo.h` there.

One pin for both repositories, so a host that links its own Flecs (the `host` feature of `flecs-c`, SPEC.md "Host
worlds") runs this library's glue against the same headers and the same code it was tested with. When the game moves
its pin, copy its two files here in the same change, and the other way round.

What this pin does not have that later Flecs does (scripts and tests here are written for the pin):

- the type shorthand `struct P(x: f32)`, `enum E(A, B)`, `vector V(type: u8)`. Write `struct P {\n  x = f32\n}`,
  `enum E {\n  A, B\n}`, `V = vector: {type: u8}`, an inline array member as `m = member: {type: i32, count: 2}`
  and an enum constant's value as `A { (constant, i32): {5} }`;
- a range check on integer literals: past u64 the value saturates at u64::MAX (strtoull without ERANGE) instead of
  being refused. Native and wasm agree.

Licence: MIT (Sander Mertens), `LICENSE`.
