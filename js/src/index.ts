// Run Flecs scripts in a world compiled to WebAssembly and read the world back as DAG-CBOR (SPEC.md).
// Works wherever WebAssembly does: browsers, Bun, Node, Cloudflare Workers. The module imports nothing.

import * as dagCbor from "@ipld/dag-cbor";
import { CID } from "multiformats/cid";
import { sha256 } from "multiformats/hashes/sha2";

/** The format version this reader understands. Anything else is refused. */
export const VERSION = 1;

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
 * link to raw blocks; read it with `readBlob`). */
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

/** The Ephemeral parts under the roots (SPEC.md, "Sync modes"): what a presence or cursor channel sends. */
export interface EphemeralMessage {
  version: 1;
  layouts: Record<string, LayoutRecord>;
  entities: { id: Uint8Array | null; path: string; tags: Ref[]; pairs: Pair[]; components: Record<string, ComponentValue> }[];
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
      await walk(get, path, ka, kb, layouts, out);
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

// Strict reading (SPEC.md, "Strictness"): exactly the keys the format defines, at every level. The layout hashes are
// checked by the Rust reader; here only their shape is.

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

const isStruct = (l: unknown) => typeof l === "object" && l !== null && "struct" in l;

/** A layout; `covered` inside a value one rule merges whole, where no member carries a rule. */
function checkLayout(l: unknown, covered: boolean): void {
  if (typeof l === "string") {
    if (l === "uptr" || l === "iptr") throw new Error(`${l} is refused in layouts`);
    return;
  }
  if (typeof l !== "object" || l === null) throw new Error("a layout is neither a primitive name nor a map");
  if ("struct" in l) {
    const m = exact(l, ["struct", "members"], "struct layout");
    for (const x of list(m.members, "members")) {
      const [n, ml, r] = list(x, "member") as [unknown, unknown, unknown];
      if (typeof n !== "string" || list(x, "member").length !== 3) throw new Error("a member is not [name, layout, rule]");
      const rule = checkRule(r, `member ${n}`);
      if (covered && rule) throw new Error(`member ${n}: has a rule inside a value merged whole`);
      if (!covered && !rule && !isStruct(ml)) throw new Error(`member ${n}: has no merge rule`);
      checkLayout(ml, covered || rule !== null);
    }
  } else if ("enum" in l) {
    const m = exact(l, ["enum", "underlying", "constants"], "enum layout");
    checkLayout(m.underlying, true);
  } else if ("bitmask" in l) exact(l, ["bitmask", "constants"], "bitmask layout");
  else if ("array" in l) checkLayout(exact(l, ["array", "count"], "array layout").array, true);
  else if ("vector" in l) checkLayout(exact(l, ["vector"], "vector layout").vector, true);
  else throw new Error(`unknown layout kind with keys ${Object.keys(l).join(", ")}`);
}

function checkLayoutRecords(v: unknown): void {
  if (typeof v !== "object" || v === null) throw new Error("no layouts map");
  for (const [path, rec] of Object.entries(v)) {
    const what = `layout ${path}`;
    const m = exact(rec, ["layout", "merge", "sync", "writable_by", "validated_by", "hash"], what);
    const merge = checkRule(m.merge, `${what}: merge`);
    if (isStruct(m.layout) === (merge !== null)) throw new Error(`${what}: merge must be null exactly for a struct`);
    checkLayout(m.layout, merge !== null);
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
}

/** An entity record in its document or block form, with exactly the keys SPEC.md defines. */
export function checkEntity(v: unknown, form: "document" | "block", layouts: Layouts): void {
  const base = ["id", "name", "tags", "pairs", "components", "isa", "inherited"];
  const m = exact(v, [...base, ...(form === "document" ? ["path", "parent"] : ["children"])], "entity record");
  if (m.id !== null && !(m.id instanceof Uint8Array && m.id.length === 16)) throw new Error("entity id is not 16 bytes or null");
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
  } else {
    for (const c of list(m.children, "children")) {
      const x = list(c, "child");
      if (x.length !== 2 || typeof x[0] !== "string" || !(x[1] instanceof CID) || x[1].code !== DAG_CBOR) {
        throw new Error("a child is not [segment, dag-cbor link]");
      }
    }
  }
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
