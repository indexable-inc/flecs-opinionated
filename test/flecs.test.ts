import { describe, expect, test } from "bun:test";
import { dlopen, FFIType, ptr, toArrayBuffer } from "bun:ffi";
import * as dagCbor from "@ipld/dag-cbor";

import { checkDocument, cidOf, diff, Flecs, MemoryBlockStore, RAW, readBlob, readCar, type Entity } from "../js/src/index.ts";
import { CID } from "multiformats/cid";

const root = new URL("..", import.meta.url).pathname;
const wasmPath = process.env.FLECS_WASM ?? `${root}dist/flecs.wasm`;
const site = await Bun.file(`${root}test/scenes/site.flecs`).text();
const flecs = await Flecs.load(await Bun.file(wasmPath).bytes());

function world(code: string, name = "site.flecs") {
  const w = flecs.world();
  w.run(name, code);
  return w;
}

function stableId(script: string, path: string): Uint8Array {
  const h = new Bun.CryptoHasher("sha256").update(`flecs-entity-id/1\0${script}\0${path}`).digest();
  return new Uint8Array(h.buffer, h.byteOffset, 16).slice();
}

const byPath = (es: Entity[]) => Object.fromEntries(es.map((e) => [e.path, e]));

describe("document", () => {
  test("is byte-identical across worlds", () => {
    using a = world(site);
    using b = world(site);
    expect(a.snapshotBytes(["site", "people"])).toEqual(b.snapshotBytes(["site", "people"]));
  });

  test("realistic scene: structs, prefabs, pairs, ordered children", () => {
    using w = world(site);
    const doc = w.snapshot(["site", "people"]);
    expect(doc.version).toBe(1);
    expect(doc.entities.map((e) => e.path)).toEqual([
      "site", "site.zeta", "site.alpha", "site.alpha.notes", "people", "people.alice", "people.bob",
    ]);
    const e = byPath(doc.entities);
    const alpha = e["site.alpha"]!;
    expect(alpha.parent).toBe(0);
    // Stable ids: SHA-256("flecs-entity-id/1\0" + script + "\0" + path), first 16 bytes.
    expect(alpha.id).toEqual(stableId("site.flecs", "site.alpha"));
    expect(alpha.isa).toEqual([{ id: stableId("site.flecs", "Page") }]);
    expect(alpha.components["Title"]!.value).toEqual({ text: "Alpha second" });
    expect(alpha.components["Status"]!.value).toBe("Review");
    // Card is shared from the Page prefab (OnInstantiate, Inherit); Status was copied, then overridden.
    expect(alpha.inherited.components).toEqual(["Card"]);
    expect(alpha.components["Card"]!.value).toEqual({
      pos: { x: 1.5, y: -2 },
      views: 18446744073709551615n,
      labels: ["page"],
      corners: [-1, 1],
      flags: ["Pinned"],
    });
    expect(alpha.pairs).toEqual([[{ id: stableId("site.flecs", "Author") }, { id: e["people.alice"]!.id! }]]);
    // Every entity has a 16-byte stable id.
    for (const x of doc.entities) expect(x.id?.length).toBe(16);
    // Member rule overrides the component rule.
    const card = doc.layouts["Card"]!.layout as { members: [string, unknown, string | null][] };
    expect(card.members.map((m) => [m[0], m[2]])).toEqual([
      ["pos", "LastWriter"], ["views", "Counter"], ["labels", "LastWriter"], ["corners", "LastWriter"], ["flags", "LastWriter"],
    ]);
    expect(doc.layouts["Title"]!.layout).toEqual({ struct: "Title", members: [["text", "string", "Text"]] });
    expect(doc.layouts["Card"]!.hash.length).toBe(8);
  });

  test("edge values", () => {
    using w = world(
      [
        "Ints = vector: {type: i32}",
        "struct E {\n  a = i64\n  b = u64\n  c = f64\n  d = string\n  e = bool\n  f = i8\n  v = Ints\n  s = string\n}",
        "x { E: {a: -9223372036854775808, b: 18446744073709551615, c: -0.5, d: \"héllo\\n\", e: true, f: -128, v: [], s: \"\"} }",
      ].join("\n"),
    );
    const bytes = w.snapshotBytes(["x"]);
    const doc = dagCbor.decode<{ entities: Entity[] }>(bytes);
    expect(doc.entities[0]!.components["E"]!.value).toEqual({
      a: -9223372036854775808n, b: 18446744073709551615n, c: -0.5, d: "héllo\n", e: true, f: -128, v: [], s: "",
    });
    // No integral floats here, so a JS round trip gives the same bytes.
    expect(dagCbor.encode(doc)).toEqual(bytes);
  });

  test("errors carry the script name and line", () => {
    using w = flecs.world();
    expect(() => w.run("bad.flecs", "a {}\nb {\n  Nope: {1}\n}\n")).toThrow(/^bad\.flecs: 3: /);
    w.run("ok.flecs", "c {}");
    expect(w.snapshot(["c"]).entities.length).toBe(1);
  });
});

describe("blocks", () => {
  test("CIDs recomputed in JS match", async () => {
    using w = world(site);
    const set = w.blocks(["site", "people"]);
    expect(set.blocks[0]![0].equals(set.root)).toBe(true);
    for (const [cid, bytes] of set.blocks) expect((await cidOf(bytes)).equals(cid)).toBe(true);
  });

  test("insertion order does not change the root", () => {
    using a = world("people {\n  alice {}\n  bob {}\n  carol {}\n}");
    using b = world("people {\n  carol {}\n  alice {}\n  bob {}\n}");
    expect(a.blocks(["people"]).root.toString()).toBe(b.blocks(["people"]).root.toString());
  });

  test("diff finds exactly the changed entities", async () => {
    const base = "struct P {\n  v = i32\n}\nroot {\n  a { P: {1} }\n  b { P: {2} }\n  c { d {} }\n  big { x {}; y {}; z {} }\n}";
    const next = "struct P {\n  v = i32\n}\nroot {\n  a { P: {1} }\n  b { P: {3} }\n  c { e {} }\n  big { x {}; y {}; z {} }\n}";
    using wa = world(base);
    using wb = world(next);
    const [x, y] = [wa.blocks(["root"]), wb.blocks(["root"])];
    const store = new MemoryBlockStore();
    store.putAll(x);
    store.putAll(y);
    const reads: string[] = [];
    const d = await diff(x.root, y.root, (cid) => (reads.push(cid.toString()), store.get(cid)));
    expect(d).toEqual([
      { path: "root.b", kind: "changed" },
      { path: "root.c.d", kind: "removed" },
      { path: "root.c.e", kind: "added" },
    ]);
    // Two roots, root, b, c on both sides: unchanged subtrees (a, big) are never read.
    expect(reads.length).toBe(8);
  });
});

describe("api", () => {
  test("roots, set, ephemeral", () => {
    using w = world(site);
    expect(w.roots()).toEqual(["Author", "Card", "Flags", "Labels", "Page", "Status", "Title", "Vec2", "people", "site"]);
    w.set("site.zeta", "Title", { text: "Zeta again" });
    expect(byPath(w.snapshot(["site"]).entities)["site.zeta"]!.components["Title"]!.value).toEqual({ text: "Zeta again" });
    expect(() => w.set("site.zeta", "Title", { txt: "x" })).toThrow(/unknown member/);
    w.run("cursor.flecs", "struct Cursor {\n  x = f32\n}\nCursor { (Sync, Ephemeral) }\nsite { Cursor: {3} }");
    expect(w.snapshot(["site"]).layouts["Cursor"]).toBeUndefined();
    expect(w.ephemeral(["site"]).entities[0]!.components["Cursor"]!.value).toEqual({ x: 3 });
  });

  test("blobs are raw blocks read lazily", async () => {
    using w = world("struct File {\n  body = sync.Blob\n}\nf { File: {body: []} }");
    const body = new Uint8Array(300_000).map((_, i) => (i * 2654435761) >>> 24);
    w.set("f", "File", { body });
    const set = w.blocks(["f"]);
    const store = new MemoryBlockStore();
    store.putAll(set);
    for (const [cid, bytes] of set.blocks) expect((await cidOf(bytes, cid.code)).equals(cid)).toBe(true);
    expect(set.blocks.some(([c]) => c.code === RAW)).toBe(true);
    const link = byPath(w.snapshot(["f"]).entities)["f"]!.components["File"]!.value as { body: CID };
    expect(await readBlob(link.body, (c) => store.get(c))).toEqual(body);
  });

  test("unknown keys are refused", () => {
    using w = world(site);
    const doc = dagCbor.decode<Record<string, unknown>>(w.snapshotBytes(["site"]));
    const entities = doc.entities as Record<string, unknown>[];
    entities[0]!.extra = 1;
    expect(() => checkDocument(doc)).toThrow(/unknown key "extra"/);
  });
});

describe("native", () => {
  const ext = process.platform === "darwin" ? "dylib" : "so";
  const lib = process.env.FLECS_NATIVE ?? `${process.env.CARGO_TARGET_DIR ?? `${root}target`}/release/libflecs_snapshot.${ext}`;
  test("integer overflow: native and wasm agree (D1)", () => {
    const { symbols: n } = dlopen(lib, {
      world_new: { args: [], returns: FFIType.ptr },
      world_free: { args: [FFIType.ptr], returns: FFIType.void },
      world_run: { args: [FFIType.ptr, FFIType.ptr, FFIType.u64, FFIType.ptr, FFIType.u64], returns: FFIType.i32 },
      world_document: { args: [FFIType.ptr, FFIType.ptr, FFIType.u64], returns: FFIType.i32 },
      world_output: { args: [FFIType.ptr, FFIType.ptr], returns: FFIType.ptr },
    });
    const enc = new TextEncoder();
    const native = (code: string): [number, Uint8Array] => {
      const w = n.world_new()!;
      const [name, c, roots] = [enc.encode("int.flecs"), enc.encode(code), enc.encode("x")];
      let rc = n.world_run(w, ptr(name), name.length, ptr(c), c.length);
      if (rc === 0) rc = n.world_document(w, ptr(roots), roots.length);
      const len = new BigUint64Array(1);
      const p = n.world_output(w, ptr(len))!;
      const out = Number(len[0]) ? new Uint8Array(toArrayBuffer(p, 0, Number(len[0]))).slice() : new Uint8Array();
      n.world_free(w);
      return [rc, out];
    };
    const wasm = (code: string): [number, Uint8Array] => {
      using w = flecs.world();
      try {
        w.run("int.flecs", code);
        return [0, w.snapshotBytes(["x"])];
      } catch (e) {
        return [1, new TextEncoder().encode((e as Error).message)];
      }
    };
    const head = "struct U {\n  v = u64\n}\nstruct I {\n  v = i64\n}\n";
    for (const body of [
      "x { U: {99999999999999999999} }",
      "x { I: {-99999999999999999999} }",
      "x { I: {99999999999999999999} }",
      "x { U: {18446744073709551615}\n I: {-9223372036854775808} }",
      "x { U: {18446744073709551616} }",
    ]) {
      const [a, b] = [native(head + body), wasm(head + body)];
      expect(b[0]).toBe(a[0]);
      expect(b[1]).toEqual(a[1]);
    }
  });

  test("native and wasm give the same bytes and root CID", () => {
    const { symbols: n } = dlopen(lib, {
      world_new: { args: [], returns: FFIType.ptr },
      world_free: { args: [FFIType.ptr], returns: FFIType.void },
      world_run: { args: [FFIType.ptr, FFIType.ptr, FFIType.u64, FFIType.ptr, FFIType.u64], returns: FFIType.i32 },
      world_document: { args: [FFIType.ptr, FFIType.ptr, FFIType.u64], returns: FFIType.i32 },
      world_blocks: { args: [FFIType.ptr, FFIType.ptr, FFIType.u64], returns: FFIType.i32 },
      world_output: { args: [FFIType.ptr, FFIType.ptr], returns: FFIType.ptr },
    });
    const enc = new TextEncoder();
    const w = n.world_new()!;
    const output = () => {
      const len = new BigUint64Array(1);
      const p = n.world_output(w, ptr(len))!;
      return new Uint8Array(toArrayBuffer(p, 0, Number(len[0]))).slice();
    };
    const [name, code, roots] = [enc.encode("site.flecs"), enc.encode(site), enc.encode("site\npeople")];
    expect(n.world_run(w, ptr(name), name.length, ptr(code), code.length)).toBe(0);
    expect(n.world_document(w, ptr(roots), roots.length)).toBe(0);
    const doc = output();
    expect(n.world_blocks(w, ptr(roots), roots.length)).toBe(0);
    const car = output();
    n.world_free(w);

    using ww = world(site);
    expect(doc).toEqual(ww.snapshotBytes(["site", "people"]));
    expect(readCar(car).root.toString()).toBe(ww.blocks(["site", "people"]).root.toString());
    expect(car).toEqual(ww.carBytes(["site", "people"]));
  });
});
