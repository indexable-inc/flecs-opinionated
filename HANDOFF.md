# Handoff

**State:** commit 959ccd4 (review fixes and work items 1 to 6) is on main, and CI run 40159500571 is green (build,
cargo test 17, bun test 12 including native vs wasm, rustfmt, clippy native/all-features/wasm32, ruff, pyright,
no-imports check, Lean proofs).

Build and test with `uv run build.py --test` on a dev host. dev-aws-8 has no lane directory any more; a new lane sets
up its own (clang/llvm-ar for wasm32, bun, uv, and binaryen 133 as `WASM_OPT`).

**Wasm size:** before this lane, 1.57 MB raw and 461 KB gzipped with no wasm-opt. Now the cargo output is 1.71 MB
(the merge crate, FastCDC, the strict reader and the writer added about 140 KB), and `wasm-opt -Os` takes it to
1.35 MB and 480 KB gzipped. `-Oz` saves only 6 KB more.

## Review of 736e38c

| Item | Status | Where |
|---|---|---|
| C1, C2 enums by underlying signedness | fixed, tests `unsigned_enum_constants`, `signed_enum_raw_zero_is_refused` | `walk.rs` `constant_values` |
| C3 isa layers sorted whole | fixed, test `isa_layers_sorted_by_path` | `walk.rs` `isa` |
| C4 ids on the script error path | fixed | `world.rs` `run` |
| C5 duplicate stable ids | fixed (assign_ids and walk), test `duplicate_stable_ids_are_refused` | `world.rs` `assign_ids`, `walk.rs` `collect` |
| C6 leading `#` | fixed: escaped as `\#`; paths resolve by segment, never `ecs_lookup`; test `hash_names_do_not_collide_with_unnamed_children` | `walk.rs` `escape`, `resolve` |
| C7 nested rules | fixed: a rule covers its whole value, inner slots are null; readers refuse inconsistent slots; SPEC "Rules"; test `nested_struct_under_a_rule_is_covered` | `walk.rs` `layout_in`, `check.rs` `layout_in` |
| C8 inherited parts | fixed: only `(OnInstantiate, Inherit)`; the multi-base order difference with Flecs (DFS by id) is documented | `walk.rs` `record`, SPEC "Entity record" |
| C9 DontFragment | refused with an error | `walk.rs` `run`/`record`, `glue_with` |
| C10 member paths in errors | done (`member pos.x`, `pts[2].x`), Rust walk and checker | `walk.rs` `member_err` |
| D1 strtoull/strtoll | saturate with ERANGE; S5 `%j` intmax_t; S6 no LLONG_MIN negation; test `integer_overflow_is_refused` and bun "integer overflow: native and wasm agree" | `wasm-libc/libc.c` |
| D2 uptr/iptr | refused in layouts (walk and readers) | `walk.rs`, `check.rs`, `index.ts` |
| S1, S2, S3 | null handle returns -1/null, `*len` written unaligned (and may be null), `buf_free(null)` is a no-op | `ffi.rs`, `world.h` |
| S7 libc exports | documented, not hidden (README "Limits") | |
| M1, M2, M3, M4 | log drained around walks; `ecs_fini` on Schema failure; thread note on `World::new` and in world.h; `-0.0` kept, SPEC and test `negative_zero_is_kept` agree | `world.rs`, SPEC "Encoding" |

## Work items

| Item | Status | Files | Next |
|---|---|---|---|
| 1. merge crate | done: `Merger` removed; `merge_state(rule, a, b)` and `merge_fields(rules, a, b)` call `merge::merge`/`merge_fields` over a strict DAG-CBOR state encoding (SPEC "Merge states"). `Text` is refused (no merge function yet). | `merge.rs`, test `merge_states` | Applying merged states to a world. Map the crate's u64 actor and u64 register values to our 32-byte actor keys and field values. |
| 2. strict records, wasm-opt | done: `check.rs` (Rust) and `checkDocument`/`checkEntity` (TS) refuse unknown keys at every level, check layouts, rule slots, layout hashes (Rust recomputes them, TS checks shape) and values. `load_entity`, `diff` and `snapshot()` use them. wasm-opt in build.py and CI (binaryen version_133, sha256 pinned). | `check.rs`, `dag.rs`, `index.ts`, `build.py`, `ci.yaml` | None. |
| 3a. `World::roots()` | done (sorted bytewise, without `flecs` and `sync`) | `world.rs`, `world_roots`, TS `roots()` | |
| 3b. `World::set(path, component, value)` | done: checked against the layout (`check::value`), written through the Flecs meta cursor, copied with `ecs_set_id` (hooks run). Refs by id or path; Blob fields take bytes. | `world.rs` `Writer`, `glue.c` `glue_writer_*`, `world_set`, TS `set()` | |
| 3c. exports behind a feature | done: `ffi` feature, off by default | `Cargo.toml`, `lib.rs` | |
| 3d. pair order | kept sorted by encoding: Flecs keeps no declared pair order (an entity's type is sorted by id), so there is nothing to expose. SPEC says to model order as data. | SPEC "Entity record" | |
| 3e. children without OrderedChildren | kept sorted by segment; Flecs's order is creation order and differs between worlds. Documented. | SPEC "Document" | |
| 4. Bytes and Blob | done: `sync.Bytes` and `sync.Blob` are `vector<u8>` types in the sync module (layouts `"bytes"`, `"blob"`). Blob: FastCDC 2020 (`fastcdc =5.0.0`, 16/64/256 KiB), single raw block or a dag-cbor chunk list; CAR and `blocks` carry raw blocks; `read_blob`/`readBlob` fetch lazily. Test `bytes_and_blobs` shows a one-byte edit makes one new chunk. | `walk.rs` `blob`, `dag.rs`, `index.ts`, SPEC "Blobs" | JS writer side has no chunker (JS passes bytes to `set`, Rust chunks). |
| 5. sync modes | done: `(Sync, Synced/LatestOnly/Ephemeral)`, `"sync"` in the layout record and hash; Ephemeral parts and layouts never in documents or blocks; `World::ephemeral` / `world_ephemeral` / TS `ephemeral()`. LatestOnly is recorded; it changes nothing until history exists. | `mode.rs`, `walk.rs`, `dag.rs`, SPEC "Sync modes", test `sync_modes` | Honour LatestOnly when the change history is built. |
| 6. bbtree | rejected for now (below) | | Revisit with the (hlc, actor) history index. |

### bbtree 0.1.5 (crates.io, published 2026-10-22)

- It builds for wasm32-unknown-unknown (`unsync::BBTreeMap`).
- It is not dependency-free: it pulls `vstd = "=0.0.0-2026-08-30-0159"` (Verus), which brings `verus_builtin_macros`
  and syn, quote and proc-macro2 at build time.
- On dev-aws-8 (c6i.24xlarge), with `(hlc, actor)` keys inserted in clock order with skew, against `BTreeMap`: at 1M
  keys, inserts took 46 ms vs 85 ms, point gets 19.9 ms vs 8.9 ms (2.2x slower), and 1000 range scans of 100
  1.29 ms vs 1.46 ms. At 10k keys the shape was the same.
- Nothing in the code needs an ordered map today. `MemoryStore` and the stable-id index are `HashMap`s (lookups are
  exact), and the history index and the MST are not built. So I did not adopt it and added no benchmark to the
  repo. The history index's pattern (append, then "everything after h") is where it would win.

### Chunker

GitHub and crates.io have fastcdc (2.45M downloads, MIT, zero dependencies), gearhash, seqcdc-rs, clast-rs and
xet-cdc-rs. I adopted fastcdc 5.0.0's 2020 variant, pinned exactly, because its cut points define blob CIDs. The
layout is a one-level IPLD Flexible Byte Layout, so other IPLD tools can read the chunk list.

## Renderer

The renderer's shims (`site/renderer/core/src/scripts.rs`) can go:

- use `World::roots()` instead of the second raw world with `glue_roots`;
- use `World::set` instead of baking a script;
- the `world_*` exports no longer leak into its cdylib, because they are behind the `ffi` feature.

## Open questions

- I added `"sync"` to layout records without bumping VERSION, because nothing is published yet. Strict readers mean
  any later key needs a version bump.
- The merge crate names actors by u64 and register values by u64; change blocks name actors by 32-byte key. The
  mapping between them isn't decided.
- Flecs crashes on `"#0" { P: {1} }` in a script, and reads a quoted `"#5"` as id 5. Should the library refuse
  `#digits` names before running a script?
- Hide the libc shim exports (malloc, strtod, ...) in the wasm? It is documented, not done.
- Should the TS reader recompute layout hashes? That needs async SHA-256 in browsers.

## Host

dev-aws-8: `~/flecs-opinionated` is deleted, and the host-ledger row is RELEASED.
