# flecs-opinionated

This library compiles [Flecs](https://github.com/SanderMertens/flecs) with a small typed interface: it runs Flecs
scripts in a world and reads the world back as deterministic DAG-CBOR. The output comes in two forms:

- one document;
- a Merkle DAG with one block per entity and one root CID. Equal subtrees have equal CIDs, so diffs skip them.

One Rust crate builds it for every target:

- **wasm32-unknown-unknown.** It has no WASI and no imports, so it runs in browsers, Bun, Node, Cloudflare Workers
  or any wasm host. The TS package `@indexable/flecs-opinionated` wraps it.
- **Native.** An rlib, a cdylib and a staticlib, with the C header `crates/flecs-snapshot/include/world.h`.

For the same scripts, native and wasm write the same bytes, and a test checks this.

The format, including the designs for merging, history, permissions and validation, is in [SPEC.md](SPEC.md).

The merge rules are pure Rust in `merge/`, proven in Lean 4 through Aeneas in `lean/` ([lean/README.md](lean/README.md)).

## TypeScript

```ts
import { Flecs, diff, MemoryBlockStore } from "@indexable/flecs-opinionated";

const flecs = await Flecs.load(); // the bundled flecs.wasm; or bytes, a URL, a Response, a WebAssembly.Module
using world = flecs.world();      // worlds are independent; many can coexist

world.run("site.flecs", `
  struct Title {
    text = string
  }
  Title { text { (Merge, Text) } }
  site {
    OrderedChildren
    intro { Title: {text: "Hello"} }
  }
`); // throws "site.flecs: 3: unresolved identifier ..." on errors

const doc = world.snapshot(["site"]);  // typed: { version, layouts, entities }
const bytes = world.snapshotBytes(["site"]);
const a = world.blocks(["site"]);      // { root: CID, blocks: [CID, bytes][], car }

world.run("edit.flecs", `site { intro { Title: {text: "Hi"} } }`);
const b = world.blocks(["site"]);
const store = new MemoryBlockStore();
store.putAll(a);
store.putAll(b);
await diff(a.root, b.root, (cid) => store.get(cid)); // [{ path: "site.intro", kind: "changed" }]
```

On Cloudflare Workers, import the wasm as a module (`import wasm from "@indexable/flecs-opinionated/flecs.wasm"`)
and pass it to `Flecs.load(wasm)`.

A world also has `roots()` (the named root paths, sorted), `set(path, component, value)` (one component value,
checked against its layout; a Blob field takes the bytes) and `ephemeral(roots)` (the `(Sync, Ephemeral)` parts,
which snapshots leave out).

Other exports:

- `readCar(bytes)`;
- `cidOf(bytes, codec?)`;
- `readBlob(link, getBlock)`, which fetches a Blob field's chunks when asked;
- `checkDocument(value)` and `checkEntity(...)`, the strict readers `snapshot()` and `diff` use;
- `checkLayouts(doc, known)`, which refuses layouts the reader was not written for;
- the format types (`Document`, `Entity`, `Layout`, `Ref`, ...).

Hash a block's bytes as you received them. Do not decode and re-encode a block to get its CID: JS numbers lose the
difference between an integral float and an integer.

## C and Rust

```c
world *w = world_new();
if (world_run(w, name, name_len, code, code_len)) { /* world_output holds the error */ }
world_blocks(w, (const uint8_t *)"site", 4);   /* CARv1 in world_output */
size_t n; const uint8_t *car = world_output(w, &n);
world_free(w);
```

The C ABI (`world_*`, and `buf_alloc`/`buf_free` on wasm) is behind the crate's `ffi` feature, so a crate that
depends on `flecs-snapshot` does not export those symbols from its own cdylib. build.py turns it on.

From Rust, use `flecs_snapshot::World`, which has `run`, `roots`, `set`, `document`, `document_bytes`, `ephemeral`
and `blocks` (giving `Blocks::car` and `Blocks::store`). It also exports `diff`, `read_blob`, `MemoryStore`, the
strict reader `check`, `merge_state` and `merge_fields` (over the `merge` crate), and the reserved `Change` and
`Countersignature` blocks.

## What a snapshot holds

There is one record per entity:

- a stable 16-byte id;
- the name;
- tags and pairs, which refer to entities by stable id, never by Flecs id;
- component values, each tagged with its layout hash;
- the prefabs it is an instance of, nearest first;
- which parts it inherited.

Children come in declared order under `OrderedChildren`; otherwise they are sorted by name, because Flecs's own
order depends on creation order and differs between worlds. Each layout record holds:

- the type's full shape;
- its merge rules, read from `(Merge, Rule)` pairs on components and members;
- its sync mode, from `(Sync, Synced | LatestOnly | Ephemeral)`;
- the reserved write and validation restrictions.

All of it is hashed into the layout hash. Readers refuse unknown format versions, mismatched layouts and unknown
keys anywhere in a record. Binary data is `sync.Bytes` (inline) or `sync.Blob` (content-defined chunks as raw blocks,
shared between versions). SPEC.md has the exact shapes.

## Build

```
uv run build.py          # cargo (wasm + native), bun install, tsc; copies the wasm to dist/flecs.wasm
uv run build.py --test   # also cargo test, bun test, and a type check of the tests
```

Requirements:

- Rust 1.99.0, from `rust-toolchain.toml`, with the wasm32-unknown-unknown target;
- bun 1.4;
- uv. The build script is Python 3.13 with asyncio and needs nothing outside the standard library;
- an unwrapped clang and llvm-ar (clang 18 or later) for wasm, set as `CC_wasm32_unknown_unknown` and
  `AR_wasm32_unknown_unknown`. A nix cc-wrapper adds host flags and breaks the build.

Flecs is vendored in `vendor/flecs`: the amalgamation the space game links, Flecs C `6e4cd338a` (4.1.6) plus the
game's fork changes, so a host world and this library run the same Flecs (`vendor/flecs/SOURCE.md`). It is built
with the LOG, SCRIPT (with META, DOC and PARSER) and PREFAB addons. The release wasm is about 1.35 MB (480 KB gzipped), built with
`opt-level = "s"` and then `wasm-opt -Os` (binaryen version_133; set `WASM_OPT` or put it on PATH).

## Why core wasm and DAG-CBOR, not the Component Model

The interface is four calls that pass bytes in and get bytes out. Core wasm with a C ABI runs on every host listed
above today, with no bindings generator and no runtime support. Component Model support is still uneven: browsers
and Workers need jco transpilation, and each language needs its own bindings. The data itself must be a stable,
content-addressed format anyway, because it is hashed, stored, synced and diffed. WIT types would describe the call,
not the bytes on disk. DAG-CBOR is canonical, has a CID link type, and has mature libraries in Rust
(`serde_ipld_dagcbor`, `cid`) and JS (`@ipld/dag-cbor`, `multiformats`).

## Limits

- **Value types.** Opaque and map types, uptr and iptr, and DontFragment components are refused, and so are NaN and
  infinities, which DAG-CBOR forbids.
- **Child count.** More than 4096 children under one parent is refused. SPEC.md has the MST plan for lifting this.
- **Unnamed entities.** They are numbered per world (`#0`, `#1`), so they are not history-independent.
- **Full rehash.** Each snapshot rehashes the whole subtree; incremental hashing is not built.
- **Merging, history, permissions, validation and authority** are designed in SPEC.md, and the format has their
  fields, but nothing enforces them yet.
- **Names like `#5` in scripts.** Flecs reads a quoted `"#5"` entity name as the id 5, and `"#0" { P: {1} }` crashes
  Flecs. Don't name entities `#` followed by digits.
- **Extra wasm exports.** The wasm module also exports the C-library shims Flecs links against (malloc, free,
  strtod, ...). Don't call them.
- **Threads.** A world stays on the thread that made it. Flecs's log and abort hooks are process-wide and set once;
  the error log they fill is per thread, so worlds on several threads work, but make every world on one thread when
  you can, so a Flecs error can never be reported on the wrong call.

## A restricted dialect (proposal)

Flecs script is the input language, run with the real `ecs_script_run`. Some of its features make a snapshot
depend on more than the declarations:

- templates;
- reactive managed scripts;
- `for` loops over runtime data;
- string interpolation into names;
- anonymous entities.

A restricted dialect would be a lint that runs before `ecs_script_run` and refuses those features. It would allow
only declarations, struct, enum, bitmask, vector and array types, prefabs, pairs and literal values. Every entity
would then be named, and every value a literal, so the same file always gives the same blocks. This is not built.

## Prior art

On GitHub I found two related projects, but no library that turns a Flecs world into content-addressed data:

- [WEREMSOFT/c99-flecs-wasm-example](https://github.com/WEREMSOFT/c99-flecs-wasm-example), a 2020 Emscripten
  demo;
- [suleymanlaarabi/tray](https://github.com/suleymanlaarabi/tray), a JavaScriptCore runtime with Flecs.

The wasm32-unknown-unknown build without WASI is adapted from den's `vendor/hyperion-flecs-c` (MIT): the
declaration-only libc headers in `crates/flecs-c/wasm-libc`, and malloc and friends on Rust's allocator. The
permission design follows Ink & Switch's [Keyhive](https://github.com/inkandswitch/keyhive).

## License

MIT
