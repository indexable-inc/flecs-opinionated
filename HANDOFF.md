# Handoff

**State:** commit 738b649 is on main, and CI (run 40156842287) is green.

That run built:

- the wasm module;
- the native .so/.a;
- the TS package, with tsc.

It passed:

- cargo test: 6 tests, including the C/Rust struct layout check;
- bun test: 8 tests;
- rustfmt, clippy (`-D warnings`) on native and wasm32, ruff and pyright;
- the check that the wasm has no imports.

Build and test with `uv run build.py --test` on a dev host. dev-aws-8 has `~/flecs-opinionated/env.sh`, which sets the
unwrapped clang/llvm-ar for wasm32 and the bun, uv and target dirs.

**Wasm size:** 1.57 MB raw and 461 KB gzipped. Running `wasm-opt -Os` would bring that to 1.24 MB and 435 KB; it
isn't wired in yet.

## Requirements

| Requirement | Status | Files | Next |
|---|---|---|---|
| Rust core over the Flecs C, built the den way | done | `crates/flecs-c` (build.rs, csrc/glue.c, wasm-libc/, src/lib.rs); `crates/flecs-snapshot/src/{world,walk,ffi}.rs`; `include/world.h` | Hide the libc shim exports in the wasm (malloc, strtod, ...). Wire `wasm-opt` into build.py. |
| Native/wasm identity | done | `test/flecs.test.ts` ("native" test, via bun:ffi); `-ffp-contract=off`; Rust strtod in `crates/flecs-c/src/lib.rs` | Run it on macOS/aarch64 too (CI is x86_64 only). |
| DAG-CBOR Merkle blocks and diff | done | `crates/flecs-snapshot/src/dag.rs`; `js/src/index.ts` (`blocks`, `readCar`, `cidOf`, `diff`, `MemoryBlockStore`) | MST past 4096 children (SPEC "Large child sets"). Incremental rehash from Flecs table versions. |
| Versioning and layout hashes | partial | `walk.rs` (`layout_record`); `dag.rs` (`check_version`); `index.ts` (`checkVersion`, `checkLayouts`) | Readers don't yet refuse unknown keys inside records, which SPEC "Versioning" requires. There is no migration API yet; migrations should be explicit, keyed by (old hash, new hash). |
| Stable entity ids | done for scripts | `schema.rs` (`derive_id`, `Id` with `(OnInstantiate, DontInherit)`); `world.rs` (`assign_ids`) | Ids for entities an actor creates at runtime, by (actor, hlc); that rule is only written in the SPEC. |
| Merge relationship as runtime data, on components and members | done (data and validation only) | `schema.rs` (sync module script); `merge.rs` (`Rule`, `Merger` trait); `walk.rs` (resolution: member > component > nested struct > default KeepBoth). Member reopening (`Title { text { (Merge, Text) } }`) is verified on Flecs 4.1.6-662, with test coverage. | Call the other lane's `merge(rule, a, b)` from `merge/` through `Merger` once it lands. Merging itself isn't built here. |
| Change DAG | format only (reserved) | `dag.rs` (`Change`, `signing_bytes`, `Countersignature`); SPEC "History" | Applying, replaying and syncing changes. Signing with ed25519-dalek (`verify_strict`). An HLC implementation. |
| Loro vs Automerge comparison | done | SPEC "Merging" → "Engine" (recommends Loro) | Prototype the Loro mapping: each CID maps to (peer, counter) spans. |
| Permissions (signing, read encryption, Keyhive) | design plus reserved vocabulary | SPEC "Permissions"; sync module relations `CanRelay`, `CanRead`, `CanWrite`, `CanAdmin`, `WritableBy` and component `Actor(key)` in `schema.rs`; `writable_by` in the layout record | Enforcement: the pure `allowed()` check and the whiteout rule (Keyhive's), plus a Lean proof (other lane). The SIV-style sealed-block format. BeeKEM integration. Open questions are listed in the SPEC. |
| Validators (ValidatedBy, authority countersigning) | design plus reserved fields | SPEC "Validation", "Authority"; `ValidatedBy`, `Validator(name, version)`, `Authority` in `schema.rs`; `validated_by` in the layout record; `Countersignature` in `dag.rs` | A validator registry keyed by (name, version) that refuses unknown validators. Tentative/rollback state for authority subtrees. |
| AGENTS.md no-generics rule | done | `AGENTS.md` | None. |

## Open questions

- Should the built wasm be checked in, or published only in the npm package? Right now it is only in `dist/`, which
  is gitignored.
- No review has been done yet. Land first, review after: a fresh reviewer should go over `walk.rs` (inheritance and
  path rules) and `ffi.rs` (pointer safety).
- `merge/` and `lean/` belong to another lane. This lane did not touch them.

## Host

dev-aws-8 holds `~/flecs-opinionated` (src, target ~650 MB, node_modules, env.sh, gc roots). The host-ledger row is
still CLAIMED. Release it and delete the directory when this lane ends.
