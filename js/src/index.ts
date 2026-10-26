// Run Flecs scripts in a world compiled to WebAssembly and read the world back as DAG-CBOR (SPEC.md).
// Works wherever WebAssembly does: browsers, Bun, Node, Cloudflare Workers. The module imports nothing.

import * as dagCbor from "@ipld/dag-cbor";
import { CID } from "multiformats/cid";
import { sha256 } from "multiformats/hashes/sha2";

/** The format version this reader understands. Anything else is refused. */
export const VERSION = 2;

/** An entity reference: its stable id, or its path when it has none (Flecs's builtins). */
export type Ref = { id: Uint8Array } | { path: string };

/** A component value. Integers past 2^53 decode as bigint. Entity and id fields are refs (or [rel, tgt] for pairs).
 * Bytes fields are Uint8Array; Blob fields are a CID link (`readBlob`), and `World.set` takes their bytes. */
export type Value = null | boolean | number | bigint | string | Uint8Array | CID | Ref | Value[] | { [key: string]: Value };

/** A merge rule name (SPEC.md, "Merging"). */
export type Rule = "LastWriter" | "AddWins" | "Counter" | "KeepBoth" | "Text";
const RULES: readonly string[] = ["LastWriter", "AddWins", "Counter", "KeepBoth", "Text"];

/** How a type's values are kept (SPEC.md, "Sync modes"). Ephemeral types never appear in documents or blocks. */
export type SyncMode = "Synced" | "LatestOnly" | "Ephemeral";
const MODES: readonly string[] = ["Synced", "LatestOnly", "Ephemeral"];

/** The multicodec codes of the blocks: entity, root and blob chunk-list blocks are dag-cbor, blob chunks raw. */
export const DAG_CBOR = 0x71;
export const RAW = 0x55;

/** A type's layout (SPEC.md, "Layouts"). Primitive names include "bytes" (an inline byte string) and "blob" (a CID
 * link to raw blocks; read it with `readBlob`). "f16", "bf16", option, union, map and runtime come only from described
 * types ("Described types"). */
export type Layout =
  | string
  | { struct: string | null; members: [name: string, layout: Layout, rule: Rule | null][] }
  | { enum: string | null; underlying: Layout; constants: [name: string, value: number | bigint][] }
  | { bitmask: string | null; constants: [name: string, value: number | bigint][] }
  | { array: Layout; count: number }
  | { vector: Layout }
  /** A value is `[]` for none, `[v]` for some. */
  | { option: Layout }
  /** A value is a map with exactly one key, the variant's name, whose value is the payload (a struct) or null. */
  | { union: string | null; variants: [name: string, payload: Layout | null][] }
  /** A value is `[[key, value], ...]`, keys unique and sorted by their DAG-CBOR encoding. */
  | { map: [key: Layout, value: Layout] }
  /** Process-local state: the value is always null. */
  | { runtime: string }
  /** A logical type (SPEC.md, "Logical types"): the value is `of`'s, which also decides the encoding, and the rules
   * (`range`, `finite`, `unit`, `refers_to`, `validated`) restrict it. `of` is never itself logical. */
  | { logical: string | null; of: Layout; rules: Record<string, unknown> };

export interface LayoutRecord {
  /** The type's path without a `V<n>` version suffix (SPEC.md, "Layouts"); see `baseOf`. */
  base: string;
  /** The type's version: n for a path ending in `V<n>`, else 1. */
  version: number;
  layout: Layout;
  /** The whole value's rule for a non-struct type; null for structs (members carry theirs). */
  merge: Rule | null;
  sync: SyncMode;
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
  version: 2;
  layouts: Record<string, LayoutRecord>;
  entities: Entity[];
}

/** An entity's children in a block: inline [segment, CID] links up to `INLINE_CHILDREN`, past that the root of an MST
 * keyed by the segment's UTF-8 bytes (SPEC.md, "Large child sets"; read it with `readMst`). */
export type Children = [segment: string, cid: CID][] | { mst: CID };

/** The most children an entity block lists inline; past this they are an MST. */
export const INLINE_CHILDREN = 4096;

/** An entity block: the record plus its children. */
export interface EntityBlock extends EntityRecord {
  children: Children;
}

/** The world root block. */
export interface RootBlock {
  version: 2;
  layouts: Record<string, LayoutRecord>;
  roots: [path: string, cid: CID][];
}

/** The Ephemeral parts under the roots (SPEC.md, "Sync modes"): what a presence or cursor channel sends. */
export interface EphemeralMessage {
  version: 2;
  layouts: Record<string, LayoutRecord>;
  entities: { id: Uint8Array | null; path: string; tags: Ref[]; pairs: Pair[]; components: Record<string, ComponentValue> }[];
}

/** One saved entity of a host world (SPEC.md, "Host record"). */
export interface HostRecord {
  id: Uint8Array;
  name: string | null;
  parent: Ref | null;
  tags: Ref[];
  pairs: Pair[];
  components: Record<string, ComponentValue>;
  /** The direct bases, sorted by their ref's DAG-CBOR. */
  isa: Ref[];
}

/** A host world's document form (SPEC.md, "Host document"): entities sorted by id. */
export interface HostDocument {
  version: 2;
  /** The id component's path. */
  ids: string;
  layouts: Record<string, LayoutRecord>;
  entities: HostRecord[];
}

/** A host world's root block: `entities` is the root of an MST keyed by the 16 id bytes, valued by entity blocks. */
export interface HostRootBlock {
  version: 2;
  ids: string;
  layouts: Record<string, LayoutRecord>;
  entities: CID;
}

/** A host root block read and checked whole (`readHostRoot`). */
export interface HostWorld {
  ids: string;
  layouts: Record<string, LayoutRecord>;
  /** Every entity, sorted by id. */
  entities: [id: Uint8Array, record: HostRecord][];
}

/** A root CID and its blocks (root block first, then entity blocks depth first, then blob blocks). */
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
  world_ephemeral(w: number, roots: number, rootsLen: number): number;
  world_roots(w: number): number;
  world_set(w: number, path: number, pathLen: number, comp: number, compLen: number, value: number, valueLen: number): number;
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

  #call(args: (string | Uint8Array)[], f: (w: number, ...ptrs: number[]) => number): Uint8Array<ArrayBuffer> {
    const w = this.#handle();
    const x = this.#x;
    const bufs = args.map((a) => (typeof a === "string" ? enc.encode(a) : a));
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

  /** The document form, decoded and checked strictly (`checkDocument`). */
  snapshot(roots: string[]): Document {
    return checkDocument(dagCbor.decode<unknown>(this.snapshotBytes(roots)));
  }

  /** The paths of every named root entity (without `flecs` and `sync`), sorted. */
  roots(): string[] {
    const out = dec.decode(this.#call([], (w) => this.#x.world_roots(w)));
    return out === "" ? [] : out.split("\n");
  }

  /** Sets component `component` on the entity at `path` to `value`, in the component's snapshot shape (a Blob field
   * takes the bytes). Checked against the layout first; throws on a mismatch. */
  set(path: string, component: string, value: Value): void {
    this.#call([path, component, dagCbor.encode(value)], (w, pp, pl, cp, cl, vp, vl) =>
      this.#x.world_set(w, pp!, pl!, cp!, cl!, vp!, vl!),
    );
  }

  /** The Ephemeral parts under `roots` (never in snapshots or blocks). */
  ephemeral(roots: string[]): EphemeralMessage {
    const v = dagCbor.decode<EphemeralMessage>(this.#call([roots.join("\n")], (w, p, n) => this.#x.world_ephemeral(w, p!, n!)));
    return checkVersion(v);
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

/** The CIDv1 (sha2-256) of a block's bytes, dag-cbor unless `codec` says raw. Hash the bytes as received: decoding
 * and re-encoding is not guaranteed to give them back (a float that happens to be integral decodes as a JS number,
 * which re-encodes as an integer). */
export async function cidOf(bytes: Uint8Array, codec: number = DAG_CBOR): Promise<CID> {
  return CID.createV1(codec, await sha256.digest(bytes));
}

async function fetchBlock(get: GetBlock, cid: CID): Promise<Uint8Array> {
  const bytes = await get(cid);
  if (!bytes) throw new Error(`block ${cid} is missing`);
  if (cid.code !== DAG_CBOR && cid.code !== RAW) throw new Error(`block ${cid} has codec ${cid.code}, not dag-cbor or raw`);
  if (!(await cidOf(bytes, cid.code)).equals(cid)) throw new Error(`block ${cid} does not hash to its CID`);
  return bytes;
}

/** A blob field's bytes (SPEC.md, "Blobs"), fetched when asked for: one raw block, or a dag-cbor chunk list
 * `[[length, raw link], ...]` whose chunks are fetched in order. */
export async function readBlob(link: CID, get: GetBlock): Promise<Uint8Array> {
  if (link.code === RAW) return fetchBlock(get, link);
  if (link.code !== DAG_CBOR) throw new Error(`blob link ${link} is neither raw nor dag-cbor`);
  const list = dagCbor.decode<unknown>(await fetchBlock(get, link));
  if (!Array.isArray(list)) throw new Error(`blob ${link}: the chunk list is not a list`);
  const parts: Uint8Array[] = [];
  for (const c of list) {
    const ok = Array.isArray(c) && c.length === 2 && typeof c[0] === "number" && c[1] instanceof CID && c[1].code === RAW;
    if (!ok) throw new Error(`blob ${link}: a chunk is not [length, raw link]`);
    const part = await fetchBlock(get, c[1] as CID);
    if (part.length !== c[0]) throw new Error(`blob ${link}: chunk ${c[1]} is ${part.length} bytes, the list says ${c[0]}`);
    parts.push(part);
  }
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) (out.set(p, at), (at += p.length));
  return out;
}

export type GetBlock = (cid: CID) => Uint8Array | undefined | Promise<Uint8Array | undefined>;

export interface Difference {
  /** The entity's path, or `layouts.<type>` for a layout record. */
  path: string;
  /** added/removed cover the whole subtree; changed means the entity's own record differs. */
  kind: "added" | "removed" | "changed";
}

async function load(get: GetBlock, cid: CID): Promise<unknown> {
  if (cid.code !== DAG_CBOR) throw new Error(`block ${cid} is not dag-cbor`);
  return dagCbor.decode<unknown>(await fetchBlock(get, cid));
}

async function loadRoot(get: GetBlock, cid: CID): Promise<RootBlock> {
  const v = await load(get, cid);
  const m = exact(v, ["version", "layouts", "roots"], "root block");
  checkVersion(m as { version: unknown });
  checkLayoutRecords(m.layouts);
  for (const r of list(m.roots, "roots")) {
    const ok = Array.isArray(r) && r.length === 2 && typeof r[0] === "string" && r[1] instanceof CID;
    if (!ok) throw new Error("a root is not [path, link]");
  }
  return v as RootBlock;
}

async function loadEntity(get: GetBlock, cid: CID, layouts: Record<string, LayoutRecord>): Promise<EntityBlock> {
  const v = await load(get, cid);
  checkEntity(v, "block", layouts);
  return v as EntityBlock;
}

const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

/** What differs between two world roots, reading only the blocks whose CIDs differ. Sorted by path (byte order of
 * the UTF-16 code units, which matches the Rust output for ASCII paths). */
export async function diff(a: CID, b: CID, get: GetBlock): Promise<Difference[]> {
  const out: Difference[] = [];
  if (a.equals(b)) return out;
  const [ra, rb] = [await loadRoot(get, a), await loadRoot(get, b)];
  const keys = [...new Set([...Object.keys(ra.layouts), ...Object.keys(rb.layouts)])].sort(cmp);
  for (const k of keys) {
    const [x, y] = [ra.layouts[k], rb.layouts[k]];
    if (x && y) {
      if (!bytesEqual(dagCbor.encode(x), dagCbor.encode(y))) out.push({ path: `layouts.${k}`, kind: "changed" });
    } else out.push({ path: `layouts.${k}`, kind: x ? "removed" : "added" });
  }
  await walk(get, "", ra.roots, rb.roots, [ra.layouts, rb.layouts], out);
  return out;
}

type Layouts = Record<string, LayoutRecord>;

async function walk(
  get: GetBlock,
  prefix: string,
  a: [string, CID][],
  b: [string, CID][],
  layouts: [Layouts, Layouts],
  out: Difference[],
): Promise<void> {
  const [ma, mb] = [new Map(a), new Map(b)];
  for (const name of [...new Set([...ma.keys(), ...mb.keys()])].sort(cmp)) {
    const path = prefix ? `${prefix}.${name}` : name;
    const [x, y] = [ma.get(name), mb.get(name)];
    if (x && y) {
      if (x.equals(y)) continue;
      const [ea, eb] = [await loadEntity(get, x, layouts[0]), await loadEntity(get, y, layouts[1])];
      const { children: ka, ...restA } = ea;
      const { children: kb, ...restB } = eb;
      if (!bytesEqual(dagCbor.encode(restA), dagCbor.encode(restB))) out.push({ path, kind: "changed" });
      // Two MSTs with one root CID hold the same children.
      if (!Array.isArray(ka) && !Array.isArray(kb) && ka.mst.equals(kb.mst)) continue;
      await walk(get, path, await childList(get, ka, path), await childList(get, kb, path), layouts, out);
    } else out.push({ path, kind: x ? "removed" : "added" });
  }
}

/** An entity block's children as [segment, CID] links, reading and checking the MST when they are one. */
async function childList(get: GetBlock, c: Children, path: string): Promise<[string, CID][]> {
  if (Array.isArray(c)) return c;
  const entries = await readMst(get, c.mst);
  if (entries.length <= INLINE_CHILDREN) {
    throw new Error(`entity ${path}: children: an MST of ${entries.length} children, which must be inline (up to ${INLINE_CHILDREN})`);
  }
  return entries.map(([k, v]): [string, CID] => {
    if (v.code !== DAG_CBOR) throw new Error(`entity ${path}: children: the value of an MST key is not a dag-cbor link`);
    try {
      return [dec.decode(k), v];
    } catch {
      throw new Error(`entity ${path}: children: an MST key is not UTF-8`);
    }
  });
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** Bytewise order of two byte strings. */
function compareBytes(a: Uint8Array, b: Uint8Array): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) if (a[i] !== b[i]) return a[i]! - b[i]!;
  return a.length - b.length;
}

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

// Merkle Search Trees (SPEC.md, "Large child sets"): the atproto layout, fanout 4.

/** A key's depth in an MST: the leading zero bits of SHA-256(key), halved and rounded down. */
export async function mstDepth(key: Uint8Array): Promise<number> {
  const d = (await sha256.digest(key)).digest;
  let zeros = 0;
  for (const b of d) {
    if (b !== 0) {
      zeros += Math.clz32(b) - 24;
      break;
    }
    zeros += 8;
  }
  return Math.floor(zeros / 2);
}

/** An MST node as stored: `l` the subtree below the first entry, each entry's `t` the subtree after it. */
export interface MstNode {
  l: CID | null;
  e: { p: number; k: Uint8Array; v: CID; t: CID | null }[];
}

const isLink = (v: unknown) => v === null || v instanceof CID;

async function loadMstNode(get: GetBlock, cid: CID): Promise<MstNode> {
  const what = `MST node ${cid}`;
  const m = exact(await load(get, cid), ["l", "e"], what);
  if (!isLink(m.l)) throw new Error(`${what}: l is not a link or null`);
  for (const x of list(m.e, `${what}: e`)) {
    const e = exact(x, ["p", "k", "v", "t"], `${what}: an entry`);
    if (!isInt(e.p) || Number(e.p) < 0) throw new Error(`${what}: an entry's p is not an unsigned integer`);
    if (!(e.k instanceof Uint8Array)) throw new Error(`${what}: an entry's k is not bytes`);
    if (!(e.v instanceof CID)) throw new Error(`${what}: an entry's v is not a link`);
    if (!isLink(e.t)) throw new Error(`${what}: an entry's t is not a link or null`);
  }
  return m as unknown as MstNode;
}

/** Every (key, value) of the MST rooted at `root`, sorted by key, after checking every node: each block against its
 * CID, keys strictly increasing across the tree, every key at its node's depth, subtrees one depth lower, `p` the
 * exact prefix shared with the previous key, and no empty node except the root of an empty tree or one between two
 * levels. */
export async function readMst(get: GetBlock, root: CID): Promise<[key: Uint8Array, value: CID][]> {
  const out: [Uint8Array, CID][] = [];
  const top = await loadMstNode(get, root);
  if (top.e.length === 0) {
    if (top.l !== null) throw new Error(`MST node ${root}: the root has no entries, but a subtree`);
    return out;
  }
  const first = top.e[0]!;
  if (first.p !== 0) throw new Error(`MST node ${root}: the first entry's p is ${first.p}, not 0`);
  await walkMst(get, root, top, await mstDepth(first.k), out);
  return out;
}

async function walkMst(get: GetBlock, cid: CID, node: MstNode, depth: number, out: [Uint8Array, CID][]): Promise<void> {
  const what = `MST node ${cid}`;
  if (node.e.length === 0 && node.l === null) throw new Error(`${what}: an empty node that is not the root of an empty tree`);
  const sub = async (link: CID | null) => {
    if (link === null) return;
    if (depth === 0) throw new Error(`${what}: a node at depth 0 has a subtree`);
    await walkMst(get, link, await loadMstNode(get, link), depth - 1, out);
  };
  await sub(node.l);
  let prev = new Uint8Array();
  for (const [i, x] of node.e.entries()) {
    const p = Number(x.p);
    if (i === 0 && p !== 0) throw new Error(`${what}: the first entry's p is ${p}, not 0`);
    if (p > prev.length) throw new Error(`${what}: entry ${i}: p ${p} is past the previous key's ${prev.length} bytes`);
    if (p < prev.length && x.k.length > 0 && x.k[0] === prev[p]) {
      throw new Error(`${what}: entry ${i}: p ${p} is not the whole prefix shared with the previous key`);
    }
    const key = new Uint8Array(p + x.k.length);
    key.set(prev.subarray(0, p));
    key.set(x.k, p);
    const last = out[out.length - 1];
    if (last && compareBytes(last[0], key) >= 0) throw new Error(`${what}: entry ${i}: key ${hex(key)} is not after ${hex(last[0])}`);
    const d = await mstDepth(key);
    if (d !== depth) throw new Error(`${what}: entry ${i}: key ${hex(key)} has depth ${d}, in a node at depth ${depth}`);
    out.push([key, x.v]);
    prev = key;
    await sub(x.t);
  }
}

/** A host world's root block, read whole and checked: the root, the MST of entities and every entity block, whose
 * `id` must be its key. */
export async function readHostRoot(get: GetBlock, cid: CID): Promise<HostWorld> {
  const m = exact(await load(get, cid), ["version", "ids", "layouts", "entities"], "host root block");
  checkVersion(m as { version: unknown });
  if (typeof m.ids !== "string") throw new Error("host root block: ids is not text");
  checkLayoutRecords(m.layouts);
  if (!(m.entities instanceof CID)) throw new Error("host root block: entities is not a link");
  const layouts = m.layouts as Layouts;
  const entities: [Uint8Array, HostRecord][] = [];
  for (const [id, link] of await readMst(get, m.entities)) {
    const what = `host entity ${hex(id)}`;
    if (id.length !== 16) throw new Error(`${what}: the MST key is not 16 bytes`);
    if (link.code !== DAG_CBOR) throw new Error(`${what}: the block link is not dag-cbor`);
    let rec: HostRecord;
    try {
      rec = checkHostEntity(await load(get, link), layouts);
    } catch (err) {
      throw new Error(`${what}: ${(err as Error).message}`);
    }
    if (!bytesEqual(rec.id, id)) throw new Error(`${what}: the block's id is ${hex(rec.id)}`);
    entities.push([id, rec]);
  }
  return { ids: m.ids, layouts, entities };
}

/** Refuses a document whose components use a layout other than the one the reader was written for. `known` maps a
 * type path to the layout hash the reader expects; types it does not list are accepted. */
export function checkLayouts(doc: { layouts: Record<string, LayoutRecord> }, known: Record<string, Uint8Array>): void {
  for (const [type, want] of Object.entries(known)) {
    const rec = doc.layouts[type];
    if (rec && !bytesEqual(rec.hash, want)) {
      throw new Error(`layout of ${type} differs from the one this reader knows: migrate explicitly`);
    }
  }
}

// Strict reading (SPEC.md, "Versioning"): exactly the keys the format defines, at every level, well-formed layouts and
// every component value against its layout. The layout hashes are checked by the Rust reader; here only their shape is.

type Obj = Record<string, unknown>;

function exact(v: unknown, keys: string[], what: string): Obj {
  if (typeof v !== "object" || v === null || Array.isArray(v) || v instanceof Uint8Array || v instanceof CID) {
    throw new Error(`${what} is not a map`);
  }
  const got = Object.keys(v);
  for (const k of got) if (!keys.includes(k)) throw new Error(`${what} has the unknown key ${JSON.stringify(k)}`);
  for (const k of keys) if (!(k in v)) throw new Error(`${what} has no ${JSON.stringify(k)}`);
  return v as Obj;
}

function list(v: unknown, what: string): unknown[] {
  if (!Array.isArray(v)) throw new Error(`${what} is not a list`);
  return v;
}

function isInt(v: unknown): boolean {
  return typeof v === "bigint" || (typeof v === "number" && Number.isInteger(v));
}

function checkRef(v: unknown, what: string): void {
  if (typeof v === "object" && v !== null && "id" in v) {
    const m = exact(v, ["id"], what);
    if (!(m.id instanceof Uint8Array) || m.id.length !== 16) throw new Error(`${what}: id is not 16 bytes`);
  } else if (typeof exact(v, ["path"], what).path !== "string") throw new Error(`${what}: path is not text`);
}

function checkRule(v: unknown, what: string): Rule | null {
  if (v === null) return null;
  if (typeof v !== "string" || !RULES.includes(v)) throw new Error(`${what}: ${String(v)} is not a merge rule`);
  return v as Rule;
}

/** The physical layout under a logical one (SPEC.md, "Logical types"), or `l` itself. */
export function physicalOf(l: Layout): Layout {
  return typeof l === "object" && l !== null && "logical" in l ? l.of : l;
}

const isStruct = (l: unknown) => {
  const p = typeof l === "object" && l !== null && "logical" in l ? (l as unknown as { of: unknown }).of : l;
  return typeof p === "object" && p !== null && "struct" in p;
};

const LOGICAL_RULES: readonly string[] = ["finite", "range", "refers_to", "unit", "validated"];

/** The integer primitives and their ranges (SPEC.md, "Values"). */
const INT_RANGES: Record<string, [bigint, bigint]> = {
  char: [-(2n ** 7n), 2n ** 7n - 1n],
  i8: [-(2n ** 7n), 2n ** 7n - 1n],
  u8: [0n, 2n ** 8n - 1n],
  byte: [0n, 2n ** 8n - 1n],
  u16: [0n, 2n ** 16n - 1n],
  u32: [0n, 2n ** 32n - 1n],
  u64: [0n, 2n ** 64n - 1n],
  uptr: [0n, 2n ** 64n - 1n],
  i16: [-(2n ** 15n), 2n ** 15n - 1n],
  i32: [-(2n ** 31n), 2n ** 31n - 1n],
  i64: [-(2n ** 63n), 2n ** 63n - 1n],
  iptr: [-(2n ** 63n), 2n ** 63n - 1n],
};
const FLOATS: readonly string[] = ["f16", "bf16", "f32", "f64"];
const OTHER_PRIMITIVES: readonly string[] = ["bool", ...FLOATS, "string", "entity", "id", "bytes", "blob"];

const isPrimitive = (p: string) => p in INT_RANGES || OTHER_PRIMITIVES.includes(p);

function checkName(v: unknown, what: string): void {
  if (v !== null && typeof v !== "string") throw new Error(`${what} is not text or null`);
}

function checkConstants(v: unknown): void {
  for (const c of list(v, "constants")) {
    const x = list(c, "constant");
    if (x.length !== 2 || typeof x[0] !== "string" || !isInt(x[1])) throw new Error("a constant is not [name, integer]");
  }
}

/** A layout; `covered` inside a value one rule merges whole, where no member carries a rule. */
function checkLayout(l: unknown, covered: boolean): void {
  if (typeof l === "string") {
    if (!isPrimitive(l)) throw new Error(`unknown primitive layout ${JSON.stringify(l)}`);
    return;
  }
  if (typeof l !== "object" || l === null || Array.isArray(l)) throw new Error("a layout is neither a primitive name nor a map");
  if ("struct" in l) {
    const m = exact(l, ["struct", "members"], "struct layout");
    checkName(m.struct, "struct name");
    const names = new Set<string>();
    for (const x of list(m.members, "members")) {
      const [n, ml, r] = list(x, "member") as [unknown, unknown, unknown];
      if (typeof n !== "string" || list(x, "member").length !== 3) throw new Error("a member is not [name, layout, rule]");
      if (names.has(n)) throw new Error(`member ${n} is listed twice`);
      names.add(n);
      const rule = checkRule(r, `member ${n}`);
      if (covered && rule) throw new Error(`member ${n}: has a rule inside a value merged whole`);
      if (!covered && !rule && !isStruct(ml)) throw new Error(`member ${n}: has no merge rule`);
      try {
        checkLayout(ml, covered || rule !== null);
      } catch (err) {
        throw new Error(memberErr(n, (err as Error).message));
      }
    }
  } else if ("enum" in l) {
    const m = exact(l, ["enum", "underlying", "constants"], "enum layout");
    checkName(m.enum, "enum name");
    if (typeof m.underlying !== "string" || !(m.underlying in INT_RANGES)) {
      throw new Error("an enum's underlying layout is not an integer type");
    }
    checkConstants(m.constants);
  } else if ("bitmask" in l) {
    const m = exact(l, ["bitmask", "constants"], "bitmask layout");
    checkName(m.bitmask, "bitmask name");
    checkConstants(m.constants);
  } else if ("array" in l) {
    const m = exact(l, ["array", "count"], "array layout");
    if (!isInt(m.count) || BigInt(m.count as number | bigint) < 0n) throw new Error("an array count is not an unsigned integer");
    checkLayout(m.array, true);
  } else if ("vector" in l) checkLayout(exact(l, ["vector"], "vector layout").vector, true);
  else if ("option" in l) checkLayout(exact(l, ["option"], "option layout").option, true);
  else if ("union" in l) {
    const m = exact(l, ["union", "variants"], "union layout");
    checkName(m.union, "union name");
    const names = new Set<string>();
    for (const x of list(m.variants, "variants")) {
      const v = list(x, "variant");
      const [n, payload] = v;
      if (v.length !== 2 || typeof n !== "string") throw new Error("a variant is not [name, layout or null]");
      if (names.has(n)) throw new Error(`variant ${n} is listed twice`);
      names.add(n);
      if (payload === null) continue;
      if (!isStruct(payload)) throw new Error(`variant ${n}: the payload is not a struct or null`);
      try {
        checkLayout(payload, true);
      } catch (err) {
        throw new Error(`variant ${n}: ${(err as Error).message}`);
      }
    }
  } else if ("map" in l) {
    const kv = list(exact(l, ["map"], "map layout").map, "map layout's [key, value]");
    if (kv.length !== 2) throw new Error("a map layout is not [key, value]");
    checkLayout(kv[0], true);
    checkLayout(kv[1], true);
  } else if ("runtime" in l) {
    if (typeof exact(l, ["runtime"], "runtime layout").runtime !== "string") throw new Error("a runtime layout's path is not text");
  } else if ("logical" in l) {
    const m = exact(l, ["logical", "of", "rules"], "logical layout");
    checkName(m.logical, "logical name");
    if (typeof m.of === "object" && m.of !== null && "logical" in m.of) throw new Error("a logical layout's of is itself logical");
    if (!isMap(m.rules)) throw new Error("a logical layout's rules are not a map");
    const keys = Object.keys(m.rules);
    for (const k of keys) if (!LOGICAL_RULES.includes(k)) throw new Error(`unknown logical rule ${JSON.stringify(k)}`);
    if (m.logical === null && keys.length === 0) throw new Error("a logical layout with no name and no rule");
    const of = m.of;
    const prim = typeof of === "string" ? of : null;
    const r = m.rules;
    if ("range" in r) {
      const b = list(r.range, "range");
      if (b.length !== 2 || prim === null || !(prim in INT_RANGES || FLOATS.includes(prim))) {
        throw new Error("rule range is not [min, max] on a number");
      }
      for (const x of b) if (x !== null && !(isInt(x) || typeof x === "number")) throw new Error("a range bound is not a number or null");
    }
    if ("finite" in r && (r.finite !== null || prim === null || !FLOATS.includes(prim))) throw new Error("rule finite is not null on a float");
    if ("unit" in r && typeof r.unit !== "string") throw new Error("rule unit is not a path");
    if ("refers_to" in r) {
      const c = list(r.refers_to, "refers_to");
      if (c.length !== 2 || typeof c[0] !== "string" || prim !== "string") throw new Error("rule refers_to is not [catalog, names] on a string");
      for (const n of list(c[1], "catalog names")) if (typeof n !== "string") throw new Error("a catalog name is not text");
    }
    if ("validated" in r) for (const c of list(r.validated, "validated")) if (typeof c !== "string") throw new Error("a check is not a path");
    checkLayout(of, covered);
  } else throw new Error(`unknown layout kind with keys ${Object.keys(l).join(", ")}`);
}

/** Prefixes an error with a member's name, joining paths as `member pos.x` or `member pts[2].x`. */
function memberErr(name: string, e: string): string {
  if (e.startsWith("member ")) {
    const rest = e.slice("member ".length);
    return rest.startsWith("[") ? `member ${name}${rest}` : `member ${name}.${rest}`;
  }
  return `member ${name}: ${e}`;
}

const isMap = (v: unknown): v is Obj =>
  typeof v === "object" && v !== null && !Array.isArray(v) && !(v instanceof Uint8Array) && !(v instanceof CID);

function show(v: unknown): string {
  if (typeof v === "bigint") return `${v}`;
  if (v instanceof Uint8Array) return `bytes ${hex(v)}`;
  try {
    return JSON.stringify(v, (_, x: unknown) => (typeof x === "bigint" ? `${x}` : x)) ?? String(v);
  } catch {
    return String(v);
  }
}

/** A value against its (already checked) layout: the shapes SPEC.md "Values" lists, maps with exactly the members. A
 * stored blob is a raw or dag-cbor link. */
function checkValue(v: unknown, l: Layout): void {
  const each = (items: unknown[], el: Layout) =>
    items.forEach((x, i) => {
      try {
        checkValue(x, el);
      } catch (err) {
        throw new Error(memberErr(`[${i}]`, (err as Error).message));
      }
    });
  if (typeof l === "string") {
    const range = INT_RANGES[l];
    if (range) {
      if (!isInt(v)) throw new Error(`${show(v)} is not an integer`);
      const i = BigInt(v as number | bigint);
      if (i < range[0] || i > range[1]) throw new Error(`${i} does not fit ${l}`);
      return;
    }
    let ok: boolean;
    if (l === "bool") ok = typeof v === "boolean";
    else if (FLOATS.includes(l)) ok = typeof v === "number" && Number.isFinite(v);
    else if (l === "string") ok = v === null || typeof v === "string";
    else if (l === "entity") {
      if (v !== null) checkRef(v, "entity value");
      ok = true;
    } else if (l === "id") {
      if (Array.isArray(v) && v.length === 2) {
        checkRef(v[0], "relationship");
        checkRef(v[1], "target");
      } else if (v !== null) checkRef(v, "id value");
      ok = true;
    } else if (l === "bytes") ok = v instanceof Uint8Array;
    else if (l === "blob") ok = v instanceof CID && (v.code === RAW || v.code === DAG_CBOR);
    else ok = false;
    if (!ok) throw new Error(`${show(v)} is not a ${l} value`);
    return;
  }
  if ("struct" in l) {
    if (!isMap(v)) throw new Error("a struct value is not a map");
    const names = l.members.map((m) => m[0]);
    for (const k of Object.keys(v)) if (!names.includes(k)) throw new Error(`the struct value has the unknown member ${JSON.stringify(k)}`);
    for (const [n, ml] of l.members) {
      if (!(n in v)) throw new Error(`the struct value has no member ${JSON.stringify(n)}`);
      try {
        checkValue(v[n], ml);
      } catch (err) {
        throw new Error(memberErr(n, (err as Error).message));
      }
    }
  } else if ("enum" in l) {
    if (typeof v !== "string" || !l.constants.some((c) => c[0] === v)) throw new Error(`${show(v)} is not a constant of the enum`);
  } else if ("bitmask" in l) {
    const seen = new Set<string>();
    for (const x of list(v, "bitmask value")) {
      if (typeof x !== "string" || !l.constants.some((c) => c[0] === x) || seen.has(x)) {
        throw new Error(`${show(x)} is not a constant of the bitmask, or is listed twice`);
      }
      seen.add(x);
    }
  } else if ("array" in l) {
    const items = list(v, "array value");
    if (BigInt(items.length) !== BigInt(l.count)) throw new Error(`the array value has ${items.length} elements, not ${l.count}`);
    each(items, l.array);
  } else if ("vector" in l) each(list(v, "vector value"), l.vector);
  else if ("option" in l) {
    const items = list(v, "option value");
    if (items.length > 1) throw new Error(`an option value has ${items.length} elements, not 0 or 1`);
    each(items, l.option);
  } else if ("union" in l) {
    if (!isMap(v)) throw new Error("a union value is not a map");
    const keys = Object.keys(v);
    if (keys.length !== 1) throw new Error(`a union value has ${keys.length} keys, not exactly one variant`);
    const name = keys[0]!;
    const variant = l.variants.find((x) => x[0] === name);
    if (!variant) throw new Error(`${JSON.stringify(name)} is not a variant of the union`);
    const payload = variant[1];
    if (payload === null) {
      if (v[name] !== null) throw new Error(`variant ${name} has no payload, so its value must be null`);
    } else {
      try {
        checkValue(v[name], payload);
      } catch (err) {
        throw new Error(`variant ${name}: ${(err as Error).message}`);
      }
    }
  } else if ("map" in l) {
    const [kl, vl] = l.map;
    let prev: Uint8Array | null = null;
    for (const [i, e] of list(v, "map value").entries()) {
      const kv = list(e, `map entry [${i}]`);
      if (kv.length !== 2) throw new Error(`map entry [${i}] is not [key, value]`);
      try {
        checkValue(kv[0], kl);
        checkValue(kv[1], vl);
      } catch (err) {
        throw new Error(memberErr(`[${i}]`, (err as Error).message));
      }
      // Keys are unique and sorted bytewise by their DAG-CBOR encoding.
      const key = dagCbor.encode(kv[0]);
      if (prev !== null && compareBytes(prev, key) >= 0) {
        throw new Error(`map entry [${i}]: key ${hex(key)} is not after the previous key ${hex(prev)} (DAG-CBOR order, unique)`);
      }
      prev = key;
    }
  } else if ("runtime" in l) {
    if (v !== null) throw new Error(`a runtime value of ${l.runtime} is ${show(v)}, not null`);
  } else if ("logical" in l) {
    checkValue(v, l.of);
    const named = l.logical === null ? "" : ` (${l.logical})`;
    const range = l.rules.range as [unknown, unknown] | undefined;
    if (range) {
      // Integers compare exactly, floats as numbers (the bounds are in the physical type's kind).
      const num = (x: unknown) => (typeof x === "bigint" ? x : typeof x === "number" && Number.isInteger(x) && typeof l.of === "string" && l.of in INT_RANGES ? BigInt(x) : (x as number));
      const [lo, hi] = [range[0], range[1]];
      if (lo !== null && num(v) < num(lo)) throw new Error(`${show(v)} is below the minimum ${show(lo)}${named}`);
      if (hi !== null && num(v) > num(hi)) throw new Error(`${show(v)} is above the maximum ${show(hi)}${named}`);
    }
    const refers = l.rules.refers_to as [string, string[]] | undefined;
    if (refers && !refers[1].includes(v as string)) {
      throw new Error(`${show(v)} is not a name in the catalog ${refers[0]}${named}`);
    }
  } else throw new Error("unknown layout");
}

/** A type path's base and version (SPEC.md, "Layouts"): a last segment ending in `V` and a decimal number that does not
 * start with 0 gives the path without that suffix and the number (`ship.HullV12` is `ship.Hull`, 12); any other path is
 * its own base at version 1. Segments split at unescaped `.` (`\` escapes the next character). `FooV1` is refused, as
 * a second name for version 1 of `Foo`. */
export function baseOf(path: string): { base: string; version: number | bigint } {
  let start = 0;
  for (let i = 0; i < path.length; i++) {
    if (path[i] === "\\") i++;
    else if (path[i] === ".") start = i + 1;
  }
  const m = /V([1-9][0-9]*)$/.exec(path.slice(start));
  if (!m) return { base: path, version: 1 };
  const at = path.length - m[0].length;
  // The suffix's V must not be escaped (an odd run of backslashes before it), and the base's name must not be empty.
  let slashes = 0;
  while (at - slashes - 1 >= start && path[at - slashes - 1] === "\\") slashes++;
  if (at === start || slashes % 2 === 1) return { base: path, version: 1 };
  const n = BigInt(m[1]!);
  if (n === 1n) throw new Error(`type ${path}: a V1 suffix is refused, since ${path.slice(0, at)} is already version 1`);
  return { base: path.slice(0, at), version: n <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(n) : n };
}

function checkLayoutRecords(v: unknown): void {
  if (typeof v !== "object" || v === null) throw new Error("no layouts map");
  for (const [path, rec] of Object.entries(v)) {
    const what = `layout ${path}`;
    const m = exact(rec, ["base", "version", "layout", "merge", "sync", "writable_by", "validated_by", "hash"], what);
    if (typeof m.base !== "string") throw new Error(`${what}: base is not text`);
    if (!isInt(m.version) || BigInt(m.version as number | bigint) < 1n) throw new Error(`${what}: version is not a positive integer`);
    const want = baseOf(path);
    if (m.base !== want.base || BigInt(m.version as number | bigint) !== BigInt(want.version)) {
      throw new Error(`${what}: base ${JSON.stringify(m.base)} version ${String(m.version)} does not match the path (${JSON.stringify(want.base)} version ${String(want.version)})`);
    }
    const merge = checkRule(m.merge, `${what}: merge`);
    if (isStruct(m.layout) === (merge !== null)) throw new Error(`${what}: merge must be null exactly for a struct`);
    try {
      checkLayout(m.layout, merge !== null);
    } catch (err) {
      throw new Error(`${what}: ${(err as Error).message}`);
    }
    if (typeof m.sync !== "string" || !MODES.includes(m.sync)) throw new Error(`${what}: ${String(m.sync)} is not a sync mode`);
    if (m.writable_by !== null) checkRef(m.writable_by, `${what}: writable_by`);
    if (m.validated_by !== null) checkRef(m.validated_by, `${what}: validated_by`);
    if (!(m.hash instanceof Uint8Array) || m.hash.length !== 8) throw new Error(`${what}: hash is not 8 bytes`);
  }
}

function checkTyped(v: unknown, ty: string, layouts: Layouts, keys: string[]): void {
  const m = exact(v, keys, `value of ${ty}`);
  const rec = layouts[ty];
  if (!rec) throw new Error(`${ty} has no layout record`);
  if (!(m.hash instanceof Uint8Array) || !bytesEqual(m.hash, rec.hash)) throw new Error(`the value of ${ty} names a layout hash it does not have`);
  try {
    checkValue(m.value, rec.layout);
  } catch (err) {
    throw new Error(`component ${ty}: ${(err as Error).message}`);
  }
}

/** The parts entity and host records share: name, tags, pairs, components, isa. */
function checkParts(m: Obj, layouts: Layouts): void {
  if (m.name !== null && typeof m.name !== "string") throw new Error("name is not text or null");
  for (const t of list(m.tags, "tags")) checkRef(t, "tag");
  for (const p of list(m.pairs, "pairs")) {
    const x = list(p, "pair");
    if (x.length !== 2 && x.length !== 3) throw new Error("a pair is not [rel, tgt] or [rel, tgt, data]");
    checkRef(x[0], "relationship");
    checkRef(x[1], "target");
    if (x.length === 3) {
      const ty = (x[2] as Obj | null)?.type;
      if (typeof ty !== "string") throw new Error("pair data has no type");
      checkTyped(x[2], ty, layouts, ["type", "hash", "value"]);
    }
  }
  const cs = m.components;
  if (typeof cs !== "object" || cs === null) throw new Error("components is not a map");
  for (const [ty, c] of Object.entries(cs)) checkTyped(c, ty, layouts, ["hash", "value"]);
  for (const r of list(m.isa, "isa")) checkRef(r, "isa");
}

/** An entity record in its document or block form, with exactly the keys SPEC.md defines. A block's MST children are
 * only checked to be a link here; `diff` reads and checks the tree. */
export function checkEntity(v: unknown, form: "document" | "block", layouts: Layouts): void {
  const base = ["id", "name", "tags", "pairs", "components", "isa", "inherited"];
  const m = exact(v, [...base, ...(form === "document" ? ["path", "parent"] : ["children"])], "entity record");
  if (m.id !== null && !(m.id instanceof Uint8Array && m.id.length === 16)) throw new Error("entity id is not 16 bytes or null");
  checkParts(m, layouts);
  const inh = exact(m.inherited, ["tags", "components", "pairs"], "inherited");
  for (const t of list(inh.tags, "inherited tags")) checkRef(t, "inherited tag");
  for (const c of list(inh.components, "inherited components")) if (typeof c !== "string") throw new Error("inherited component is not text");
  for (const p of list(inh.pairs, "inherited pairs")) {
    const x = list(p, "inherited pair");
    if (x.length !== 2) throw new Error("an inherited pair is not [rel, tgt]");
    checkRef(x[0], "relationship");
    checkRef(x[1], "target");
  }
  if (form === "document") {
    if (typeof m.path !== "string") throw new Error("path is not text");
    if (m.parent !== null && !isInt(m.parent)) throw new Error("parent is not an index");
  } else if (Array.isArray(m.children)) {
    if (m.children.length > INLINE_CHILDREN) {
      throw new Error(`children: ${m.children.length} inline, past ${INLINE_CHILDREN}, where they must be an MST`);
    }
    for (const c of m.children) {
      const x = list(c, "child");
      if (x.length !== 2 || typeof x[0] !== "string" || !(x[1] instanceof CID) || x[1].code !== DAG_CBOR) {
        throw new Error("a child is not [segment, dag-cbor link]");
      }
    }
  } else {
    const c = exact(m.children, ["mst"], "children");
    if (!(c.mst instanceof CID) || c.mst.code !== DAG_CBOR) throw new Error("children: mst is not a dag-cbor link");
  }
}

/** A host record (SPEC.md, "Host record"), with exactly the keys it defines. */
export function checkHostEntity(v: unknown, layouts: Layouts): HostRecord {
  const m = exact(v, ["id", "name", "parent", "tags", "pairs", "components", "isa"], "host record");
  if (!(m.id instanceof Uint8Array && m.id.length === 16)) throw new Error("host record: id is not 16 bytes");
  if (m.parent !== null) checkRef(m.parent, "parent");
  checkParts(m, layouts);
  return v as HostRecord;
}

/** A whole host document, strictly: the version, the id component's path, the layout records and every host record,
 * with ids strictly increasing bytewise. */
export function checkHostDocument(v: unknown): HostDocument {
  const m = exact(v, ["version", "ids", "layouts", "entities"], "the host document");
  checkVersion(m as { version: unknown });
  if (typeof m.ids !== "string") throw new Error("the host document: ids is not text");
  checkLayoutRecords(m.layouts);
  const layouts = m.layouts as Layouts;
  let prev: Uint8Array | null = null;
  for (const [i, e] of list(m.entities, "entities").entries()) {
    let rec: HostRecord;
    try {
      rec = checkHostEntity(e, layouts);
    } catch (err) {
      throw new Error(`entity ${i}: ${(err as Error).message}`);
    }
    if (prev !== null && compareBytes(prev, rec.id) >= 0) throw new Error(`entity ${i}: id ${hex(rec.id)} is not after ${hex(prev)}`);
    prev = rec.id;
  }
  return v as HostDocument;
}

/** A whole document, strictly: the version, the layout records and every entity record. */
export function checkDocument(v: unknown): Document {
  const m = exact(v, ["version", "layouts", "entities"], "the document");
  checkVersion(m as { version: unknown });
  checkLayoutRecords(m.layouts);
  const layouts = m.layouts as Layouts;
  list(m.entities, "entities").forEach((e, i) => {
    try {
      checkEntity(e, "document", layouts);
    } catch (err) {
      throw new Error(`entity ${i}: ${(err as Error).message}`);
    }
  });
  return v as Document;
}
