// Run Flecs scripts in a world compiled to WebAssembly and read the world back as DAG-CBOR (SPEC.md).
// Works wherever WebAssembly does: browsers, Bun, Node, Cloudflare Workers. The module imports nothing.

import * as dagCbor from "@ipld/dag-cbor";
import { CID } from "multiformats/cid";
import { sha256 } from "multiformats/hashes/sha2";

/** The format version this reader understands. Anything else is refused. */
export const VERSION = 1;

/** An entity reference: its stable id, or its path when it has none (Flecs's builtins). */
export type Ref = { id: Uint8Array } | { path: string };

/** A component value. Integers past 2^53 decode as bigint. Entity and id fields are refs (or [rel, tgt] for pairs). */
export type Value = null | boolean | number | bigint | string | Uint8Array | Ref | Value[] | { [key: string]: Value };

/** A merge rule name (SPEC.md, "Merging"). */
export type Rule = "LastWriter" | "AddWins" | "Counter" | "KeepBoth" | "Text";

/** A type's layout (SPEC.md, "Layouts"). */
export type Layout =
  | string
  | { struct: string | null; members: [name: string, layout: Layout, rule: Rule | null][] }
  | { enum: string | null; underlying: Layout; constants: [name: string, value: number | bigint][] }
  | { bitmask: string | null; constants: [name: string, value: number | bigint][] }
  | { array: Layout; count: number }
  | { vector: Layout };

export interface LayoutRecord {
  layout: Layout;
  /** The whole value's rule for a non-struct type; null for structs (members carry theirs). */
  merge: Rule | null;
  /** Reserved, not enforced: the role whose holders may write this type. */
  writable_by: Ref | null;
  /** Reserved, not enforced: the validator every write must pass. */
  validated_by: Ref | null;
  /** First 8 bytes of SHA-256 over the DAG-CBOR of this record without `hash`. */
  hash: Uint8Array;
}

export interface ComponentValue {
  hash: Uint8Array;
  value: Value;
}

export type Pair = [rel: Ref, tgt: Ref] | [rel: Ref, tgt: Ref, data: ComponentValue & { type: string }];

/** One entity's own data (SPEC.md, "Entity record"). */
export interface EntityRecord {
  id: Uint8Array | null;
  name: string | null;
  tags: Ref[];
  pairs: Pair[];
  components: Record<string, ComponentValue>;
  /** Every prefab the entity is an instance of, nearest first. */
  isa: Ref[];
  /** Which of the tags, components and pairs above came from a prefab. */
  inherited: { tags: Ref[]; components: string[]; pairs: [Ref, Ref][] };
}

export interface Entity extends EntityRecord {
  path: string;
  /** Index of the parent in `entities`, or null for a root. */
  parent: number | null;
}

/** The document form: every entity under the roots, depth first. */
export interface Document {
  version: 1;
  layouts: Record<string, LayoutRecord>;
  entities: Entity[];
}

/** An entity block: the record plus its children as [segment, CID] links. */
export interface EntityBlock extends EntityRecord {
  children: [segment: string, cid: CID][];
}

/** The world root block. */
export interface RootBlock {
  version: 1;
  layouts: Record<string, LayoutRecord>;
  roots: [path: string, cid: CID][];
}

/** A root CID and its blocks (root block first, then entity blocks depth first). */
export interface BlockSet {
  root: CID;
  blocks: [CID, Uint8Array][];
  /** The CARv1 file the blocks came from. */
  car: Uint8Array;
}

/** Where blocks live. */
export interface BlockStore {
  get(cid: CID): Uint8Array | undefined | Promise<Uint8Array | undefined>;
  put(cid: CID, bytes: Uint8Array): void | Promise<void>;
}

export class MemoryBlockStore implements BlockStore {
  readonly #blocks = new Map<string, Uint8Array>();
  get(cid: CID): Uint8Array | undefined {
    return this.#blocks.get(cid.toString());
  }
  put(cid: CID, bytes: Uint8Array): void {
    this.#blocks.set(cid.toString(), bytes);
  }
  putAll(set: BlockSet): void {
    for (const [cid, bytes] of set.blocks) this.put(cid, bytes);
  }
  get size(): number {
    return this.#blocks.size;
  }
}

interface Exports {
  memory: WebAssembly.Memory;
  buf_alloc(n: number): number;
  buf_free(p: number, n: number): void;
  world_new(): number;
  world_free(w: number): void;
  world_run(w: number, name: number, nameLen: number, code: number, codeLen: number): number;
  world_document(w: number, roots: number, rootsLen: number): number;
  world_blocks(w: number, roots: number, rootsLen: number): number;
  world_output(w: number, lenOut: number): number;
}

export type Source = BufferSource | URL | string | WebAssembly.Module | Response;

async function compile(source: Source): Promise<WebAssembly.Module> {
  if (source instanceof WebAssembly.Module) return source;
  if (source instanceof Response) return WebAssembly.compile(await source.arrayBuffer());
  if (typeof source === "string" || source instanceof URL) {
    const url = new URL(source, import.meta.url);
    if (url.protocol === "file:") {
      const { readFile } = await import("node:fs/promises");
      return WebAssembly.compile(await readFile(url));
    }
    const res = await fetch(url);
    if (!res.ok) throw new Error(`fetching ${url}: HTTP ${res.status}`);
    return WebAssembly.compile(await res.arrayBuffer());
  }
  return WebAssembly.compile(source);
}

const enc = new TextEncoder();
const dec = new TextDecoder("utf-8", { fatal: true });

/** A loaded module. Each `world()` is independent; many may coexist. */
export class Flecs {
  readonly #x: Exports;

  private constructor(x: Exports) {
    this.#x = x;
  }

  /** Loads the module: bytes, a URL or path, a Response, or a compiled WebAssembly.Module (Workers). By default
   * the flecs.wasm shipped next to this file. */
  static async load(source: Source = new URL("./flecs.wasm", import.meta.url)): Promise<Flecs> {
    const instance = await WebAssembly.instantiate(await compile(source), {});
    return new Flecs(instance.exports as unknown as Exports);
  }

  world(): World {
    const w = this.#x.world_new();
    if (w === 0) throw new Error("world_new failed");
    return new World(this.#x, w);
  }
}

export class World {
  readonly #x: Exports;
  #w: number;

  /** @internal */
  constructor(x: Exports, w: number) {
    this.#x = x;
    this.#w = w;
  }

  #handle(): number {
    if (this.#w === 0) throw new Error("world is disposed");
    return this.#w;
  }

  #call(args: string[], f: (w: number, ...ptrs: number[]) => number): Uint8Array<ArrayBuffer> {
    const w = this.#handle();
    const x = this.#x;
    const bufs = args.map((a) => enc.encode(a));
    const ptrs: number[] = [];
    for (const b of bufs) {
      const p = x.buf_alloc(b.length);
      new Uint8Array(x.memory.buffer, p, b.length).set(b);
      ptrs.push(p, b.length);
    }
    const lenOut = x.buf_alloc(4);
    try {
      const rc = f(w, ...ptrs);
      const p = x.world_output(w, lenOut);
      const n = new DataView(x.memory.buffer).getUint32(lenOut, true);
      const out = new Uint8Array(x.memory.buffer, p, n).slice();
      if (rc !== 0) throw new Error(dec.decode(out));
      return out;
    } finally {
      for (let i = 0; i < ptrs.length; i += 2) x.buf_free(ptrs[i]!, ptrs[i + 1]!);
      x.buf_free(lenOut, 4);
    }
  }

  /** Runs a Flecs script. Throws with "name: line: message" on errors. */
  run(name: string, code: string): void {
    this.#call([name, code], (w, np, nl, cp, cl) => this.#x.world_run(w, np!, nl!, cp!, cl!));
  }

  /** The DAG-CBOR bytes of the document form. Byte-identical for the same scripts on every host. */
  snapshotBytes(roots: string[]): Uint8Array<ArrayBuffer> {
    return this.#call([roots.join("\n")], (w, p, n) => this.#x.world_document(w, p!, n!));
  }

  /** The document form, decoded. */
  snapshot(roots: string[]): Document {
    return checkVersion(dagCbor.decode<Document>(this.snapshotBytes(roots)));
  }

  /** The block form as a CARv1 file. */
  carBytes(roots: string[]): Uint8Array<ArrayBuffer> {
    return this.#call([roots.join("\n")], (w, p, n) => this.#x.world_blocks(w, p!, n!));
  }

  /** The block form: one root CID and its blocks. */
  blocks(roots: string[]): BlockSet {
    return readCar(this.carBytes(roots));
  }

  dispose(): void {
    if (this.#w !== 0) {
      this.#x.world_free(this.#w);
      this.#w = 0;
    }
  }

  [Symbol.dispose](): void {
    this.dispose();
  }
}

function checkVersion<T extends { version: unknown }>(v: T): T {
  if (v.version !== VERSION) throw new Error(`format version ${String(v.version)} is not supported (this reader knows ${VERSION})`);
  return v;
}

function varint(b: Uint8Array, at: number): [number, number] {
  let n = 0;
  let shift = 0;
  for (let i = at; i < b.length; i++) {
    const byte = b[i]!;
    n += (byte & 0x7f) * 2 ** shift;
    if ((byte & 0x80) === 0) return [n, i + 1];
    shift += 7;
  }
  throw new Error("truncated varint in CAR");
}

/** Reads a CARv1 file whose header names one root. */
export function readCar(car: Uint8Array): BlockSet {
  let [len, at] = varint(car, 0);
  const header = dagCbor.decode<{ version: number; roots: CID[] }>(car.subarray(at, at + len));
  if (header.version !== 1 || header.roots.length !== 1) throw new Error("expected a CARv1 file with one root");
  at += len;
  const blocks: [CID, Uint8Array][] = [];
  while (at < car.length) {
    [len, at] = varint(car, at);
    const section = car.subarray(at, at + len);
    const [cid, bytes] = CID.decodeFirst(section);
    blocks.push([cid, bytes.slice()]);
    at += len;
  }
  return { root: header.roots[0]!, blocks, car };
}

/** The CIDv1 (dag-cbor, sha2-256) of a block's bytes. Hash the bytes as received: decoding and re-encoding is not
 * guaranteed to give them back (a float that happens to be integral decodes as a JS number, which re-encodes as an
 * integer). */
export async function cidOf(bytes: Uint8Array): Promise<CID> {
  return CID.createV1(dagCbor.code, await sha256.digest(bytes));
}

export type GetBlock = (cid: CID) => Uint8Array | undefined | Promise<Uint8Array | undefined>;

export interface Difference {
  /** The entity's path, or `layouts.<type>` for a layout record. */
  path: string;
  /** added/removed cover the whole subtree; changed means the entity's own record differs. */
  kind: "added" | "removed" | "changed";
}

async function load<T>(get: GetBlock, cid: CID): Promise<T> {
  const bytes = await get(cid);
  if (!bytes) throw new Error(`block ${cid} is missing`);
  if (!(await cidOf(bytes)).equals(cid)) throw new Error(`block ${cid} does not hash to its CID`);
  return dagCbor.decode<T>(bytes);
}

const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

/** What differs between two world roots, reading only the blocks whose CIDs differ. Sorted by path (byte order of
 * the UTF-16 code units, which matches the Rust output for ASCII paths). */
export async function diff(a: CID, b: CID, get: GetBlock): Promise<Difference[]> {
  const out: Difference[] = [];
  if (a.equals(b)) return out;
  const [ra, rb] = [checkVersion(await load<RootBlock>(get, a)), checkVersion(await load<RootBlock>(get, b))];
  const keys = [...new Set([...Object.keys(ra.layouts), ...Object.keys(rb.layouts)])].sort(cmp);
  for (const k of keys) {
    const [x, y] = [ra.layouts[k], rb.layouts[k]];
    if (x && y) {
      if (!bytesEqual(dagCbor.encode(x), dagCbor.encode(y))) out.push({ path: `layouts.${k}`, kind: "changed" });
    } else out.push({ path: `layouts.${k}`, kind: x ? "removed" : "added" });
  }
  await walk(get, "", ra.roots, rb.roots, out);
  return out;
}

async function walk(get: GetBlock, prefix: string, a: [string, CID][], b: [string, CID][], out: Difference[]): Promise<void> {
  const [ma, mb] = [new Map(a), new Map(b)];
  for (const name of [...new Set([...ma.keys(), ...mb.keys()])].sort(cmp)) {
    const path = prefix ? `${prefix}.${name}` : name;
    const [x, y] = [ma.get(name), mb.get(name)];
    if (x && y) {
      if (x.equals(y)) continue;
      const [ea, eb] = [await load<EntityBlock>(get, x), await load<EntityBlock>(get, y)];
      const { children: ka, ...restA } = ea;
      const { children: kb, ...restB } = eb;
      if (!bytesEqual(dagCbor.encode(restA), dagCbor.encode(restB))) out.push({ path, kind: "changed" });
      await walk(get, path, ka, kb, out);
    } else out.push({ path, kind: x ? "removed" : "added" });
  }
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** Refuses a document whose components use a layout other than the one the reader was written for. `known` maps a
 * type path to the layout hash the reader expects; types it does not list are accepted. */
export function checkLayouts(doc: Document | RootBlock, known: Record<string, Uint8Array>): void {
  for (const [type, want] of Object.entries(known)) {
    const rec = doc.layouts[type];
    if (rec && !bytesEqual(rec.hash, want)) {
      throw new Error(`layout of ${type} differs from the one this reader knows: migrate explicitly`);
    }
  }
}
