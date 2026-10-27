import { describe, expect, test } from "bun:test";
import { dlopen, FFIType, ptr, toArrayBuffer } from "bun:ffi";
import * as dagCbor from "@ipld/dag-cbor";

import {
  baseOf,
  checkDocument,
  checkHostDocument,
  cidOf,
  diff,
  Flecs,
  MemoryBlockStore,
  RAW,
  readBlob,
  readCar,
  readHostRoot,
  readMst,
  type Entity,
} from "../js/src/index.ts";
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
    expect(doc.version).toBe(2);
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
    expect([doc.layouts["Card"]!.base, doc.layouts["Card"]!.version]).toEqual(["Card", 1]);
  });

  test("edge values", () => {
    using w = world(
      [
        "vector Ints(i32)",
        "struct E(a: i64, b: u64, c: f64, d: string, e: bool, f: i8, v: Ints, s: string)",
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
    const base = "struct P(v: i32)\nroot {\n  a { P: {1} }\n  b { P: {2} }\n  c { d {} }\n  big { x {}; y {}; z {} }\n}";
    const next = "struct P(v: i32)\nroot {\n  a { P: {1} }\n  b { P: {3} }\n  c { e {} }\n  big { x {}; y {}; z {} }\n}";
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
    w.run("cursor.flecs", "struct Cursor(x: f32)\nCursor { (Sync, Ephemeral) }\nsite { Cursor: {3} }");
    expect(w.snapshot(["site"]).layouts["Cursor"]).toBeUndefined();
    expect(w.ephemeral(["site"]).entities[0]!.components["Cursor"]!.value).toEqual({ x: 3 });
  });

  test("blobs are raw blocks read lazily", async () => {
    using w = world("struct File(body: sync.Blob)\nf { File: {body: []} }");
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
    const head = "struct U(v: u64)\nstruct I(v: i64)\n";
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

// Format 2: layout bases and versions, uptr, MST child lists, host documents and blocks.

type Store = Map<string, Uint8Array>;

async function put(store: Store, v: unknown): Promise<CID> {
  const bytes = dagCbor.encode(v);
  const cid = await cidOf(bytes);
  store.set(cid.toString(), bytes);
  return cid;
}

/** The MST depth, computed here independently of the library: leading zero bits of SHA-256(key), halved. */
function depthOf(key: Uint8Array): number {
  let z = 0;
  for (const b of new Bun.CryptoHasher("sha256").update(key).digest()) {
    if (b !== 0) return (z + Math.clz32(b) - 24) >> 1;
    z += 8;
  }
  return z >> 1;
}

const sharedPrefix = (a: Uint8Array, b: Uint8Array) => {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  return i;
};

type MstEntry = { p: number; k: Uint8Array; v: CID; t: CID | null };

/** A small MST builder (SPEC.md, "Large child sets") over entries sorted by key; returns the root CID. */
async function buildMst(store: Store, entries: [Uint8Array, CID][]): Promise<CID> {
  const items = entries.map(([k, v]) => ({ k, v, d: depthOf(k) }));
  type Item = (typeof items)[number];
  const node = async (xs: Item[], d: number): Promise<CID> => {
    const e: MstEntry[] = [];
    let l: CID | null = null;
    let seg: Item[] = [];
    let prev: Uint8Array = new Uint8Array();
    const flush = async () => {
      const sub = seg.length ? await node(seg, d - 1) : null;
      seg = [];
      if (e.length === 0) l = sub;
      else e[e.length - 1]!.t = sub;
    };
    for (const x of xs) {
      if (x.d < d) {
        seg.push(x);
        continue;
      }
      await flush();
      const p = sharedPrefix(prev, x.k);
      e.push({ p, k: x.k.slice(p), v: x.v, t: null });
      prev = x.k;
    }
    await flush();
    return put(store, { l, e });
  };
  if (items.length === 0) return put(store, { l: null, e: [] });
  return node(items, Math.max(...items.map((x) => x.d)));
}

const hexOf = (b: Uint8Array) => Buffer.from(b).toString("hex");

const record = (base: string, version: number, layout: unknown = "i32") => ({
  base, version, layout, merge: "LastWriter", sync: "Synced", writable_by: null, validated_by: null, hash: new Uint8Array(8),
});
const withLayouts = (layouts: Record<string, unknown>) => ({ version: 2, layouts, entities: [] });

describe("layouts", () => {
  test("base and version come from the path", () => {
    expect(baseOf("FooV2")).toEqual({ base: "Foo", version: 2 });
    expect(baseOf("ship.HullV12")).toEqual({ base: "ship.Hull", version: 12 });
    expect(baseOf("Foo")).toEqual({ base: "Foo", version: 1 });
    // V0... is not a version suffix, so FooV01 is its own base.
    expect(baseOf("FooV01")).toEqual({ base: "FooV01", version: 1 });
    // An escaped dot does not split segments.
    expect(baseOf("a\\.bV2")).toEqual({ base: "a\\.b", version: 2 });
    expect(() => baseOf("FooV1")).toThrow(/V1 suffix/);
  });

  test("records are checked against their path", () => {
    expect(() => checkDocument(withLayouts({ FooV2: record("Foo", 2) }))).not.toThrow();
    expect(() => checkDocument(withLayouts({ FooV01: record("FooV01", 1) }))).not.toThrow();
    expect(() => checkDocument(withLayouts({ FooV2: record("FooV2", 1) }))).toThrow(/does not match the path/);
    expect(() => checkDocument(withLayouts({ FooV2: record("Foo", 3) }))).toThrow(/does not match the path/);
    expect(() => checkDocument(withLayouts({ FooV1: record("Foo", 1) }))).toThrow(/V1 suffix/);
    expect(() => checkDocument(withLayouts({ FooV1: record("FooV1", 1) }))).toThrow(/V1 suffix/);
    expect(() => checkDocument(withLayouts({ Foo: record("Foo", 0) }))).toThrow(/positive integer/);
    const { base: _, ...noBase } = record("Foo", 1);
    expect(() => checkDocument(withLayouts({ Foo: noBase }))).toThrow(/has no "base"/);
  });

  test("a world's records carry base and version", () => {
    using w = world("struct HullV2(v: i32)\nx { HullV2: {1} }");
    const rec = w.snapshot(["x"]).layouts["HullV2"]!;
    expect([rec.base, rec.version]).toEqual(["Hull", 2]);
  });

  test("uptr and iptr are allowed", () => {
    expect(() => checkDocument(withLayouts({ P: record("P", 1, "uptr"), Q: record("Q", 1, "iptr") }))).not.toThrow();
    using w = world("struct P(v: uptr)\nx { P: {7} }");
    const doc = w.snapshot(["x"]);
    expect((doc.layouts["P"]!.layout as { members: [string, unknown, unknown][] }).members[0]![1]).toBe("uptr");
    expect(doc.entities[0]!.components["P"]!.value).toEqual({ v: 7 });
  });
});

describe("described layouts", () => {
  const circle = { struct: null, members: [["r", "f32", null]] };
  const layout = {
    struct: "W",
    members: [
      ["h", "f16", "LastWriter"],
      ["b", "bf16", "LastWriter"],
      ["o", { option: "i32" }, "LastWriter"],
      ["u", { union: "Shape", variants: [["None", null], ["Circle", circle]] }, "LastWriter"],
      ["m", { map: ["string", "u8"] }, "LastWriter"],
      ["r", { runtime: { size: 8, align: 8, implicit: false } }, "LastWriter"],
      ["q", { runtime: "phys.Body" }, "LastWriter"],
    ],
  };
  const rec = (l: unknown = layout) => ({ ...record("W", 1, l), merge: null });
  const good = { h: 0.5, b: -2, o: [7], u: { Circle: { r: 1.5 } }, m: [["b", 1], ["aa", 2]], r: null, q: null };
  const doc = (value: unknown, l: unknown = layout) => ({
    version: 2,
    layouts: { W: rec(l) },
    entities: [{
      id: new Uint8Array(16), name: "x", tags: [], pairs: [], components: { W: { hash: new Uint8Array(8), value } }, isa: [],
      inherited: { tags: [], components: [], pairs: [] }, path: "x", parent: null,
    }],
  });

  test("good values pass", () => {
    expect(() => checkDocument(doc(good))).not.toThrow();
    // None, an empty option and an empty map.
    expect(() => checkDocument(doc({ ...good, o: [], u: { None: null }, m: [] }))).not.toThrow();
  });

  test("bad values are refused", () => {
    expect(() => checkDocument(doc({ ...good, o: [1, 2] }))).toThrow(/member o: an option value has 2 elements/);
    expect(() => checkDocument(doc({ ...good, u: { None: null, Circle: { r: 1 } } }))).toThrow(/member u: a union value has 2 keys/);
    expect(() => checkDocument(doc({ ...good, u: { Square: null } }))).toThrow(/not a variant/);
    expect(() => checkDocument(doc({ ...good, u: { None: 1 } }))).toThrow(/must be null/);
    expect(() => checkDocument(doc({ ...good, u: { Circle: { r: "x" } } }))).toThrow(/variant Circle: member r/);
    // Sorted by DAG-CBOR bytes ("b" is 0x61 0x62, "aa" is 0x62 ...), not by text, and unique.
    expect(() => checkDocument(doc({ ...good, m: [["aa", 2], ["b", 1]] }))).toThrow(/member m: map entry \[1\]: key .* is not after/);
    expect(() => checkDocument(doc({ ...good, m: [["b", 1], ["b", 2]] }))).toThrow(/is not after/);
    expect(() => checkDocument(doc({ ...good, m: [["b", 300]] }))).toThrow(/does not fit u8/);
    expect(() => checkDocument(doc({ ...good, r: 0 }))).toThrow(/member r: a runtime value .* not null/);
    expect(() => checkDocument(doc({ ...good, h: "1" }))).toThrow(/member h: .* is not a f16 value/);
  });

  test("described layouts are checked strictly", () => {
    const withMember = (l: unknown) => ({ struct: "W", members: [["v", l, "LastWriter"]] });
    const bad = (l: unknown) => () => checkDocument(withLayouts({ W: rec(withMember(l)) }));
    // Inside a covered value every rule slot is null: a union payload's member may not carry a rule.
    expect(bad({ union: null, variants: [["A", { struct: null, members: [["r", "f32", "Counter"]] }]] })).toThrow(/has a rule inside/);
    expect(bad({ option: { struct: null, members: [["r", "f32", "LastWriter"]] } })).toThrow(/has a rule inside/);
    expect(bad({ map: ["string", { struct: null, members: [["r", "f32", "LastWriter"]] }] })).toThrow(/has a rule inside/);
    expect(bad({ union: null, variants: [["A", null], ["A", null]] })).toThrow(/listed twice/);
    expect(bad({ union: null, variants: [["A", "i32"]] })).toThrow(/not a struct or null/);
    expect(bad({ option: "i32", extra: 1 })).toThrow(/unknown key "extra"/);
    expect(bad({ map: ["string"] })).toThrow(/not \[key, value\]/);
    expect(bad({ runtime: null })).toThrow(/runtime layout's body is not a map/);
    expect(bad({ runtime: { size: 6, align: 4, implicit: true } })).toThrow(/power-of-two align/);
    expect(bad({ runtime: { size: 8, align: 8 } })).toThrow(/has no "implicit"/);
    expect(bad("f128")).toThrow(/unknown primitive/);
    // A whole option or map value carries the component's rule, as any non-struct does.
    expect(() => checkDocument(withLayouts({ O: record("O", 1, { option: "i32" }) }))).not.toThrow();
  });
});

describe("logical types", () => {
  const ruled = [
    "struct Ratio(num: u32, den: u32)",
    "Percent {\n  (IsA, flecs.meta.u8)\n  sync.Range: {max: 100}\n}",
    "struct Gauge(level: u8)",
    "Ratio {\n  den {\n    sync.Range: {min: 1}\n  }\n}",
    "Gauge {\n  level {\n    (IsA, Percent)\n  }\n}",
    "r {\n  Ratio: {num: 1, den: 2}\n  Gauge: {level: 50}\n}",
    "bad {\n  Gauge: {level: 101}\n}",
  ].join("\n");

  test("refined members are logical layouts the reader checks", () => {
    using w = world(ruled);
    const doc = w.snapshot(["r"]);
    const gauge = Object.values(doc.layouts).find((l) => l.base === "Gauge")!;
    expect(gauge.layout).toEqual({
      struct: "Gauge",
      members: [["level", { logical: "Percent", of: "u8", rules: { range: [null, 100] } }, "KeepBoth"]],
    });
    expect(() => checkDocument(doc)).not.toThrow();
    // The wasm world refuses a value past a rule; the reader does too, given one.
    expect(() => w.snapshot(["bad"])).toThrow(/member level: 101 is above the maximum 100 \(Percent\)/);
    const forged = structuredClone(doc);
    const r = forged.entities.find((e) => e.path === "r")!;
    (r.components["Ratio"]!.value as { den: number }).den = 0;
    expect(() => checkDocument(forged)).toThrow(/member den: 0 is below the minimum 1/);
  });
});

describe("mst", () => {
  const enc = new TextEncoder();

  async function tree(n: number) {
    const store: Store = new Map();
    const entries: [Uint8Array, CID][] = [];
    for (let i = 0; i < n; i++) entries.push([enc.encode(`child-${i}`), await cidOf(dagCbor.encode(i))]);
    entries.sort((a, b) => Buffer.compare(a[0], b[0]));
    return { store, entries, root: await buildMst(store, entries) };
  }

  test("round-trips 5000 keys", async () => {
    const { store, entries, root } = await tree(5000);
    expect(store.size).toBeGreaterThan(100);
    const got = await readMst((c) => store.get(c.toString()), root);
    expect(got.map(([k, v]) => [hexOf(k), v.toString()])).toEqual(entries.map(([k, v]) => [hexOf(k), v.toString()]));
  });

  test("an empty tree is one node", async () => {
    const store: Store = new Map();
    expect(await readMst((c) => store.get(c.toString()), await buildMst(store, []))).toEqual([]);
  });

  test("tampered trees are refused", async () => {
    const { store, entries, root } = await tree(5000);
    const get = (s: Store) => (c: CID) => s.get(c.toString());

    // A block whose bytes no longer hash to its CID.
    const [victim, bytes] = [...store.entries()].find(([k]) => k !== root.toString())!;
    const bad = new Map(store);
    const flipped = bytes.slice();
    flipped[0] = flipped[0]! ^ 1;
    bad.set(victim, flipped);
    await expect(readMst(get(bad), root)).rejects.toThrow(/does not hash to its CID/);

    const at = (d: number) => entries.filter(([k]) => depthOf(k) === d);
    const [a, b] = at(0) as [[Uint8Array, CID], [Uint8Array, CID]];
    const [c] = at(1) as [[Uint8Array, CID]];
    const entry = (k: Uint8Array, v: CID, prev?: Uint8Array): MstEntry => {
      const p = prev ? sharedPrefix(prev, k) : 0;
      return { p, k: k.slice(p), v, t: null };
    };
    const s: Store = new Map();

    // Two depth-0 keys out of order.
    const swapped = await put(s, { l: null, e: [entry(b[0], b[1]), entry(a[0], a[1], b[0])] });
    await expect(readMst(get(s), swapped)).rejects.toThrow(/is not after/);

    // A depth-1 key in a depth-0 node.
    const [x, y] = Buffer.compare(a[0], c[0]) < 0 ? [a, c] : [c, a];
    const mixed = await put(s, { l: null, e: [entry(x[0], x[1]), entry(y[0], y[1], x[0])] });
    await expect(readMst(get(s), mixed)).rejects.toThrow(/has depth/);

    // An empty node that is not the root of an empty tree.
    const empty = await put(s, { l: null, e: [] });
    const hollow = await put(s, { l: empty, e: [entry(c[0], c[1])] });
    await expect(readMst(get(s), hollow)).rejects.toThrow(/empty node/);

    // A prefix count that is not the whole shared prefix (every key starts with "child-").
    const loose = await put(s, { l: null, e: [entry(a[0], a[1]), { p: 0, k: b[0], v: b[1], t: null }] });
    await expect(readMst(get(s), loose)).rejects.toThrow(/whole prefix/);
  });

  test("diff reads a large child set as an MST", async () => {
    const kids = (changed: number) =>
      Array.from({ length: 5000 }, (_, i) => `  c${i} { P: {${i === changed ? -1 : i}} }`).join("\n");
    using wa = world(`struct P(v: i32)\nroot {\n${kids(-1)}\n}`);
    using wb = world(`struct P(v: i32)\nroot {\n${kids(1234)}\n}`);
    const [x, y] = [wa.blocks(["root"]), wb.blocks(["root"])];
    const store = new MemoryBlockStore();
    store.putAll(x);
    store.putAll(y);
    for (const [cid, bytes] of x.blocks) expect((await cidOf(bytes, cid.code)).equals(cid)).toBe(true);
    const top = dagCbor.decode<{ roots: [string, CID][] }>(store.get(x.root)!);
    const rootBlock = dagCbor.decode<{ children: unknown }>(store.get(top.roots[0]![1])!);
    expect(rootBlock.children).toEqual({ mst: expect.any(CID) });
    expect(await diff(x.root, y.root, (c) => store.get(c))).toEqual([{ path: "root.c1234", kind: "changed" }]);
  });
});

describe("host", () => {
  const id = (n: number) => new Uint8Array(16).map((_, i) => (i === 15 ? n : 0));
  const host = (n: number, parent: number | null = null) => ({
    id: id(n),
    name: `e${n}`,
    parent: parent === null ? null : { id: id(parent) },
    tags: [{ path: "game.Marker" }],
    pairs: [],
    components: {},
    isa: [],
  });

  test("host documents are checked strictly", () => {
    const doc = (entities: unknown[]) => ({ version: 2, ids: "game.Uid", layouts: { FooV2: record("Foo", 2) }, entities });
    expect(checkHostDocument(doc([host(1), host(2, 1)])).entities.length).toBe(2);
    expect(() => checkHostDocument(doc([host(2), host(1)]))).toThrow(/entity 1: id .* is not after/);
    expect(() => checkHostDocument(doc([host(1), host(1)]))).toThrow(/is not after/);
    expect(() => checkHostDocument(doc([{ ...host(1), inherited: { tags: [], components: [], pairs: [] } }]))).toThrow(
      /unknown key "inherited"/,
    );
    expect(() => checkHostDocument(doc([{ ...host(1), id: null }]))).toThrow(/id is not 16 bytes/);
    expect(() => checkHostDocument({ ...doc([]), version: 1 })).toThrow(/format version 1/);
  });

  test("host root blocks are read through their MST", async () => {
    const store: Store = new Map();
    const get = (c: CID) => store.get(c.toString());
    const rootOf = async (pairs: [number, number][]) => {
      const entries: [Uint8Array, CID][] = [];
      for (const [key, rec] of pairs) entries.push([id(key), await put(store, host(rec, rec === 1 ? null : 1))]);
      entries.sort((a, b) => Buffer.compare(a[0], b[0]));
      return put(store, { version: 2, ids: "game.Uid", layouts: {}, entities: await buildMst(store, entries) });
    };
    const w = await readHostRoot(get, await rootOf([[1, 1], [2, 2], [3, 3]]));
    expect(w.ids).toBe("game.Uid");
    expect(w.entities.map(([k, r]) => [k[15], r.name])).toEqual([[1, "e1"], [2, "e2"], [3, "e3"]]);
    await expect(readHostRoot(get, await rootOf([[1, 1], [3, 2]]))).rejects.toThrow(/the block's id/);
  });
});
