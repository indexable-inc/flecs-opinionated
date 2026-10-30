# Handoff

## Flecs master port (lane FLECS-PORT, 2026-10-29)

**State:** on branch `flecs-master-c73`, not pushed, **never compiled**: this lane wrote code by reading only (no
cargo, bun, build.py or bake). The integrator builds once and fixes what is left; the list below says where to look.

- **Fork model.** This repository has one Flecs C, `vendor/flecs/distr`, copied unchanged from a pinned commit of
  [indexable-inc/flecs](https://github.com/indexable-inc/flecs) `master`. That branch is always upstream
  SanderMertens/flecs `master` plus our open upstream PRs rebased onto it; each PR has its own branch on the fork.
  No local Flecs C patches. Today the pin is `c73ce7115`, identical to upstream (no open PRs).
  `uv run scripts/bump_flecs.py --flecs <checkout>` fetches upstream and the fork, rebases the fork's `master`,
  copies `distr`, regenerates the bindings, records both commits in `vendor/flecs/SOURCE.md` and prints the
  `git push --force-with-lease fork master` to run after review (it never pushes).
- **Crates.** `crates/flecs_ecs`, `flecs_ecs_sys` and `flecs_ecs_derive` (Flecs-Rust `bf956db99`) are workspace
  crates, maintained here; `flecs_ecs_sys` compiles `vendor/flecs/distr`. Their `SOURCE.md` files list every change,
  and `crates/flecs_ecs/SOURCE.md` "Port to Flecs master" lists the Rust API changes users must make (defer takes
  the stage, `with`/`slot_of`/`DefaultChildComponent`/`TableIter::to_str` gone, `Override` is `Copy`, get callbacks
  lock tables instead of deferring; new `ordered_children`, script `reload`/`undo`, the script shell).
- **Snapshot.** flecs-snapshot saves and restores `Parent` children in their order and `DontFragment` parts on host
  worlds (SPEC.md, `host-check/tests/hierarchy.rs`).
- **Integrator: first build.** Regenerate `Cargo.lock` and `host-check/Cargo.lock`. Places most likely not to
  compile:
  - `flecs_ecs_sys/src/flecs_rust.c`: master internals (`flecs_entities_try`, `flecs_commands_end`, the `ecs_check`
    label in `ecs_rust_record_find`), and the hand-added `ecs_rust_record_find` in both bindings files;
  - `flecs_ecs_sys/build.rs`: the `ALWAYS` defines and the `flecs_safety_locks` `compile_error!`;
  - `flecs_ecs`: `World::defer` (closure lifetime, `self.world()`), `ScriptEntityView` reaching `self.world`
    through `Deref`, `addons/script/shell.rs` (`ecs_vec_first` on an empty vec), `flecs::Copy` shadowing
    `std::marker::Copy` inside `core::flecs`, the `TableLockGuard` re-exports, `TableColumnMut::locked`, and the
    dead `flecs_safety_locks` cfg code (`safety_map.rs`, `utility/safety.rs`, cfg blocks in `get_tuple.rs`,
    `cloned_tuple.rs`), which names sys items master does not have;
  - `flecs-c/csrc/glue.c`: the host defines and `glue_ordered_children` (`flecs_components_get`,
    `flecs_component_get_flags`); flecs-snapshot `walk.rs` (~579), `host.rs` (~1388, ~1470) and
    `host-check/tests/hierarchy.rs`;
  - `scripts/bump_flecs.py`: bindgen doc post-processing; run `--bindings-only` and diff against the commit.
- **Behaviour to check.** Master locks tables inside OnAdd/OnRemove hooks, and a structural change inside a get
  callback or table guard is no longer queued (it asserts in debug, and release builds do not check). Observers
  built without terms match the Any term.
- **Section below** "Host worlds": its Pin paragraph describes the game's 4.1.6 setup before this lane.

## Logical types (lane FLECS-LOGICAL, 2026-10-26)

**State:** on main. Described and script types split into a physical layout (decides encoding and Flecs meta) and
logical refinements (SPEC.md "Logical types", "Compatibility"). `uv run build.py --test`, host-check, rustfmt and the
four clippy runs passed on dev-aws-8.

- Format: new layout kind `{"logical": name|null, "of": L, "rules": {...}}` (rules `range`, `finite`, `unit`,
  `refers_to`, `validated`), additive within VERSION 2. An unrefined layout is byte-identical, so its hash is
  unchanged. `NonZero<T>` is now the logical type over `T`, so its value is the bare integer. Its old shape was the
  game's struct `{"0": n}`, so hashes of types that hold one change.
- Code:
  - `crates/flecs-snapshot/src/logical.rs`: the node, the rules, checks and their Flecs data.
  - `compat.rs`: `compare` returns `Safe`, `NeedsCheck` or `NeedsMigration`.
  - `migrate.rs`: `Migrations::compatible`.
  - `walk.rs`: script member and type rules through IsA.
  - `schema.rs`: the vocabulary.
  - `flecs-describe`: `catalog.rs` (`Catalog`), the NonZero impls, `__private::{refine, CHECKS}`.
  - The derive's `range`, `unit`, `finite`, `validate` and `logical`.
  - `js/src/index.ts`: the reader accepts the kind and checks range and catalog.
- Enforcement: save, restore and `set` refuse with the member path and never clamp. `validated` checks run only
  where a world is: on restore and set, not on save, and not in the readers.
- Game changes 1-11 from its SOURCE.md are upstream: the flecs-c build.rs finds `vendor/flecs_ecs_sys` too. One
  fix on top: a runtime part's `Default` probe ran inside a generic fn, so it always said "no Default". The derive
  now expands it at the field's type. The game's vendored copy has this bug.
- Tests:
  - flecs-snapshot `script_member_rules`, `compat_table`;
  - flecs-describe `tests/logical.rs`;
  - host-check `restore_refuses_a_broken_rule`;
  - bun "logical types".
- Open:
  - "Entity reference saved as a stable id, restored as a relation": the `entity` primitive already saves a stable
    id; a relation-valued member was not built.
  - REST and Script writes cannot be intercepted (Flecs's `on_validate` hook only suppresses OnSet); they are
    refused at the next save.
  - A `Counter` merge on a ranged member can leave the range.
  - u64 range bounds past 2^53 lose precision (`sync.Range` is f64).
  - A generic derived type's `validate` runs only in `from_value`, so no check entity is made for it.
- dev-aws-8: `~/flecslog` deleted, ledger row RELEASED.

**State:** the host-world work (lane FLECS-OPINIONATED, 2026-10-25) is on main: `Host` (save, incremental blocks,
restore, live migration) on a host's own Flecs world, described types (`flecs-describe`, `#[derive(Describe)]`), and
`host-check/`, a host on flecs_ecs that tests both. `uv run build.py --test` passed on dev-aws-8 (cargo wasm and
native, bun test, tsc, host-check, cargo test), with ruff, pyright, rustfmt and the four clippy runs clean.

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
| M1, M2, M3, M4 | log drained around walks; `ecs_fini` on Schema failure; thread note on `World::new` and in world.h; `-0.0` is kept in the `Ipld` document (test `negative_zero_is_kept`) but its bytes are those of `0.0`, as `serde_ipld_dagcbor` writes it and refuses `-0.0` on decode (SPEC "Encoding", corrected 2026-10-25) | `world.rs`, `cbor.rs`, SPEC "Encoding" |

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

## Host worlds (2026-10-25)

What the space game needs to replace `crates/space-snapshot` (its model: the-game `docs/agents/rust-live.md`).

- **Pin.** The game: Flecs 4.1.6 (`6e4cd338a`) plus its fork (a copy in `vendor/flecs_ecs_sys/src`; this library's own
  `vendor/flecs` has moved to upstream master, `vendor/flecs/SOURCE.md`); flecs_ecs and flecs_ecs_sys at rev
  `bf956db99b69d41ee142c9d52054911af28d7e93`. `host-check` patches flecs_ecs_sys to `vendor/flecs_ecs_sys` (that
  pin built from our amalgamation): the git flecs_ecs_sys lacks the fork's symbols (`EcsParentDepth`), so the root
  workspace's `--all-features` does not link; CI and build.py list features instead.
- **Linking.** `flecs-c` with `host` links the host's flecs_ecs_sys. It must be in the same artifact as
  flecs_ecs_sys: the game's optimised profile builds with `-fvisibility=hidden`.
- **Coverage.** The game's 2797 component types (738 tags, 2059 with data): 1269 fit Flecs meta today, all 2797 are
  described by the derive with 0 refusals, 56 of them with a `#[describe(runtime)]` part.
- **Numbers** (dev-aws-8, Zen 4, loaded; 6805 entities, `host-check` `bench_game_size`): document 1,582,316 bytes,
  120,805 zstd-3; encode 17.2 ms (was 167); `blocks_full` 46 ms; unchanged rehash 10.5 ms; 1% changed 14.4 ms;
  `blocks_new` at 1% 10.6 ms (358 blocks); restore 80 to 92 ms (was 374); `migrate_live` 44 to 62 ms.
  space-snapshot on the M5 Max: 160,921 bytes zstd, encode 9.9 + 3.3 ms, decode 1.5 + 166 ms.
- **Game lane steps.** Derive `Describe` on each component and call `flecs_describe::register::<T>` after
  flecs_ecs registers it; lock layouts by committing the layout records' hashes; `Host::new(world.ptr_mut().cast(),
  "Uid", "Persist")`; `encode` to save, `blocks`/`blocks_new` for the state hash, `restore` on load, a
  `Migrations` edge plus `migrate_live` for each `FooV2`.
- **Not done.** Restore of migrated values goes through `Ipld` (the direct path covers only values whose layout is
  live). The raw-bytes cache sees an entity member by its local id (SPEC "Incremental hashing").

## Host

dev-aws-8: `~/flecsop` is deleted, and the host-ledger row is RELEASED.
