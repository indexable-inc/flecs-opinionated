# Format

Version 1. This file describes what the library writes, what a reader must check, and the parts that are designed
but not built yet (merging, history, permissions, validation). Sections marked **reserved** describe fields that are
in the format now but are not checked or acted on.

## Encoding

Every byte string the library produces is [DAG-CBOR](https://ipld.io/specs/codecs/dag-cbor/spec/). That gives the
canonical form of RFC 8949 §4.2.1 plus IPLD's stricter rules:

- map keys are strings, sorted by length first and then bytewise;
- integers use the shortest form;
- floats are always 64-bit; NaN and the infinities are not allowed, so writing one is an error that names the
  component and member. `-0.0` is kept as written (DAG-CBOR allows it, and it is a different float from `0.0`), so
  `-0.0` and `0.0` give different bytes;
- the only tag is 42 (a CID link);
- no indefinite lengths.

The same scripts produce the same bytes on every host. Native and wasm builds compile the same Flecs C with
`-ffp-contract=off`, so arithmetic in script expressions rounds the same way. In wasm, strtod is Rust's correctly
rounded parser. A test compares native and wasm output byte for byte.

A block's CID is CIDv1 with multihash sha2-256 (0x12). Entity, root and blob chunk-list blocks use codec dag-cbor
(0x71); blob chunks are raw (0x55).

Integer literals in scripts that do not fit 64 bits are an error on every host: the wasm libc's `strtoull` and
`strtoll` saturate and set `ERANGE`, as native libc does, so Flecs refuses the same literals natively and in wasm.

## Values

| Flecs type | Value |
|---|---|
| bool | bool |
| char, u8 to u64, i8 to i64, byte | integer |
| uptr, iptr | refused: their width is the host's pointer width, which differs between native and wasm |
| `sync.Bytes` (a `vector<u8>`) | a byte string, inline (CBOR major type 2) |
| `sync.Blob` (a `vector<u8>`) | a link (tag 42) to the blob's blocks; see "Blobs" |
| f32, f64 | float (an f32 is widened to f64 exactly) |
| string | text, or null for a null pointer; it must be UTF-8 |
| entity | a ref, or null for 0 |
| id | a ref, `[ref, ref]` for a pair, or null |
| enum | the constant's name; a value that is not a constant is an error. A constant's value is Flecs's `value_unsigned` when the underlying type is unsigned and `value` otherwise |
| bitmask | an array of constant names; leftover bits are an error |
| struct | a map from member name to value |
| array, or an inline member array (`count > 1`) | an array |
| vector | an array |
| opaque, map, anything else | an error that names the type |
| a component marked `DontFragment` | an error: its values live outside the entity's type, where the walk does not read them yet |

Errors name the component and the member path inside it, such as `component Card: member pos.x: ...` or
`member pts[2].x`.

Binary data uses the two `vector<u8>` types the `sync` module defines, so the choice is in the type, and so in the
layout (`"bytes"` or `"blob"`): `struct File(name: string, thumb: sync.Bytes, body: sync.Blob)`. Use Bytes for small
values that change together with the rest of the record, and Blob for large ones, so an unchanged body is never
re-hashed into a new entity block and unchanged chunks are shared between versions. Only `LastWriter` and `KeepBoth`
apply to them.

A **ref** is `{"id": bytes16}` for an entity with a stable id, or `{"path": text}` for one without (Flecs's
builtins, such as `flecs.core.Prefab`). Snapshots never contain Flecs entity ids.

Paths are Flecs paths. Each segment is the entity's name with `\` and `.` escaped by `\`, and a leading `#` escaped
the same way, so a child named `#0` (possible through the C API; a script reads `"#0"` as an id) is `\#0` and never
collides with an unnamed child. An unnamed child is `#n`, where n counts the parent's unnamed children in Flecs id
order. That ordering is local to one world, so unnamed entities are not history-independent; name anything that will
be compared across worlds.

The library resolves the paths it is given (roots, `World::set`, `{"path"}` refs) the same way it writes them:
segment by segment against the segments above. It never searches parent scopes or Flecs's lookup path, as
`ecs_lookup` does.

## Stable ids

Every entity outside `flecs` and `sync` gets a 16-byte id the first time a script run sees it:

    id = SHA-256("flecs-entity-id/1" 0x00 || script name || 0x00 || path)[0..16]

`path` is the entity's path at that moment. The id is stored in the `sync.Id` component and never recomputed, so
renaming or moving an entity keeps its id. Two peers that run the same script under the same name give its entities
the same ids, which is what lets them merge the entity instead of duplicating it. `sync.Id` has
`(OnInstantiate, DontInherit)`: a prefab instance gets its own id from its own path rather than the prefab's.

A script that fails part way still has made the entities before the error, and they get ids too, so no user entity
is ever left without one. Two entities with the same id (a script that copies `sync.Id`, or a second script under
the same name making an entity at a path an earlier one had) are an error, both when ids are assigned and in every
snapshot that holds both.

Pair targets, entity-typed fields and `isa` lists refer to entities by this id.

Not built yet: entities created at runtime by an actor rather than by a script. They will use
`SHA-256("flecs-entity-id/1" 0x00 || actor key || 0x00 || hlc)[0..16]`, which is unique per actor and per clock
tick.

## Layouts

Each component and pair type a snapshot uses gets one layout record, keyed by the type's path:

    {"layout": L, "merge": rule | null, "sync": mode, "writable_by": ref | null, "validated_by": ref | null,
     "hash": bytes8}

`hash` is the first 8 bytes of SHA-256 over the DAG-CBOR of the record without `hash`. Every component value carries
the hash of its type's record, so a value can never be read with the wrong layout.

`L` is one of:

- a primitive name: `"bool"`, `"u8"` … `"f64"`, `"string"`, `"entity"`, `"id"`, `"bytes"`, `"blob"` and so on
  (`"uptr"` and `"iptr"` are refused);
- `{"struct": path | null, "members": [[name, L, rule | null], ...]}`;
- `{"enum": path | null, "underlying": L, "constants": [[name, int], ...]}`;
- `{"bitmask": path | null, "constants": [[name, uint], ...]}`;
- `{"array": L, "count": n}`;
- `{"vector": L}`.

Constants and members are listed in declaration order. Renaming a member, reordering members, changing a type,
changing a rule or changing the sync mode all change the hash.

`merge` is the whole value's rule for a non-struct type and null for a struct. A member's rule slot follows "Rules"
below: inside a value one rule merges whole, every rule slot is null. A reader refuses a record that breaks this.

## Entity record

    {"id": bytes16 | null, "name": text | null,
     "tags": [ref], "pairs": [[rel, tgt] | [rel, tgt, {"type": path, "hash": bytes8, "value": v}]],
     "components": {type path: {"hash": bytes8, "value": v}},
     "isa": [ref],
     "inherited": {"tags": [ref], "components": [path], "pairs": [[rel, tgt]]}}

- **tags and pairs** are sorted by their DAG-CBOR encoding. Flecs keeps no declaration order for them (an entity's
  type is sorted by id, and ids follow creation order), so there is no declared order to keep; when order matters,
  model it as data (a vector component, or children under `OrderedChildren`). The record leaves out the name, the
  parent, `IsA`, the stable id and ids that carry flags.
- **Builtin components** (anything under `flecs`) are written as tags with no value. Their layouts depend on the
  pointer size, so leaving the values out keeps wasm32 and 64-bit output identical.
- **`isa`** lists every prefab the entity is an instance of, a whole depth at a time, nearest first. Each depth is
  sorted by path: `e : A, B`, `A : Z`, `B : Y` gives `[A, B, Y, Z]`.
- **Inherited parts** are the tags, components and pairs whose type (or relationship) has `(OnInstantiate, Inherit)`
  and that the entity has through `isa` but does not own: exactly what Flecs's `ecs_get` and `ecs_has` find through
  IsA. A part with the default `(OnInstantiate, Override)` is copied into the instance, so it is owned; one with
  `DontInherit` is not reachable at all. When two bases give the same part, the first in `isa` order wins. Flecs
  instead searches depth first in the order of the bases' entity ids, which is not stable across worlds, so the two
  differ only for a part given by two bases at once; give it to one base, or own it on the instance.
- **Skipped parts**: `Prefab`, anything without `(OnInstantiate, Inherit)` on a base, and a second target of an
  exclusive relationship.

## Document

    {"version": 1, "layouts": {path: record}, "entities": [record + {"path": text, "parent": index | null}]}

Entities are in depth-first order from each root in the order the caller listed them. Children come in declared
order when the parent has `OrderedChildren`, and otherwise sorted by segment (bytewise). Flecs's own child order
without `OrderedChildren` is table and id order, which depends on the order entities were created and is not the
same across worlds, so it is not used. `World::roots()` lists the named roots (without `flecs` and `sync`), also
sorted bytewise.

Ephemeral parts (see "Sync modes") and their layouts are never in a document.

## Blocks

Each entity is one block: its record plus `"children": [[segment, CID], ...]`, in the child order above. The ChildOf
tree is the Merkle tree. The world root block is:

    {"version": 1, "layouts": {path: record}, "roots": [[path, CID], ...]}

The library writes a block set as a CARv1 file: the header `{"roots": [root CID], "version": 1}`, then the root block,
then the entity blocks depth first, then the blob blocks in the order the walk reached them, each written once.

### Blobs

A `sync.Blob` value is cut with FastCDC 2020 (the `fastcdc` crate 5.0.0, pinned exactly, normalization level 1, seed
0) with a minimum of 16 KiB, an average of 64 KiB and a maximum of 256 KiB per chunk. Each chunk is a raw block. The
field holds:

- the raw chunk's link, when the blob is one chunk (including the empty blob);
- otherwise the link of a dag-cbor chunk list `[[length, raw link], ...]` in order.

This is the IPLD Flexible Byte Layout restricted to one level. Because chunk boundaries depend on the content, an
edit changes only the chunks around it, and every other chunk keeps its CID and is shared between versions. A blob of
more than 16384 chunks (about 1 GiB) is refused for now. A different chunker or different sizes would change every
blob CID, so they are part of this version.

Readers fetch blobs lazily: the entity block holds only the link, and `read_blob` (Rust) or `readBlob` (TS) fetches
and checks the chunks when asked. `diff` never reads blob blocks; a changed blob shows as a changed entity.

`World::set` takes a Blob field's bytes and does the chunking. A Blob cannot be Ephemeral, because it needs stored
blocks; use Bytes there.

### Diff

`diff(a, b, getBlock)` compares two roots. Subtrees with equal CIDs are skipped without being read. A subtree that
exists on only one side is reported once, at its top, as `added` or `removed`. An entity whose own record differs is
reported as `changed`. Every block read is checked against its CID.

### Large child sets

The child list is stored inline. Its CID depends only on its contents, not on the order the children were inserted
(the sort is by segment), so the format is already history-independent. The cost is that each change to one child
rewrites the parent's whole list. A parent with more than 4096 children is refused for now.

The plan is to replace the inline list, past that threshold, with a Merkle Search Tree in the atproto layout
(`{"mst": CID}` in place of the list; `@atcute/mst` on the JS side). In an MST a node's position comes from hashing
its key, which keeps it history-independent while making one insert cost O(log n) blocks. I found no Rust MST crate I
could use:

- atrium-repo needs tokio;
- rsky-repo pulls in secp256k1 and much of a PDS;
- `merkle-search-tree` is not IPLD-shaped.

So that part will be written here.

### Incremental hashing

Not built. Every snapshot walks and hashes the whole subtree; hashing is cheap next to the walk. The plan is to cache
each entity's CID and recompute only entities whose tables changed (Flecs table versions), plus their ancestors.

## Versioning

- Every document, root block, change block and countersignature carries `"version"`. A reader refuses any version it
  does not know, and so does `diff`. The library never guesses.
- Every component value carries its layout hash. A reader built for certain layouts checks them (`checkLayouts` in
  TS) and refuses a mismatch instead of reading the fields wrongly.
- Readers are strict: every map in a document, root block, entity block or layout record must have exactly the keys
  this spec lists, layouts must be well formed (rule slots as in "Rules", known primitive names, hash recomputed),
  and every value must fit its layout. Anything else is refused, because an unknown key may change what the record
  means. `check::document` in Rust and `checkDocument` in TS do this; the TS reader checks the layout hashes' shape
  and leaves recomputing them to the Rust reader. Strict readers mean no key can be added within version 1.
- Anything else needs a new version.
- Migrations are explicit code, written for one (old layout hash → new layout hash) pair. Nothing converts
  implicitly.

## The sync module

Every world starts with a `sync` module, defined by a Flecs script (`crates/flecs-snapshot/src/schema.rs`) and put
on the lookup path, so scripts write `(Merge, Text)` without the prefix. It holds:

- the `Id` component;
- the `Merge` relationship and its rules;
- the `Sync` relationship and its modes;
- the `Bytes` and `Blob` types;
- the reserved permission and validation vocabulary below.

## Merging

The rules are data in the world, and the code reads them as runtime values: `merge::Rule` is an enum read from the
world, never a type parameter. The `merge` crate in this repository (`merge/`, proven in Lean in `lean/`) implements
the merge functions behind one non-generic `merge(rule, a, b)`. This crate calls it with `merge::merge_state` and
`merge::merge_fields`, over the state encoding in "Merge states". `Text` has no merge function yet and is refused
there. Applying changes to a world is not built.

### Rules

`Merge` is an exclusive relationship. Its targets are:

| Rule | Meaning |
|---|---|
| `LastWriter` | The write with the greatest (HLC, actor) wins. |
| `AddWins` | A set (vector) where an add survives a concurrent remove. |
| `Counter` | Concurrent increments add up. |
| `KeepBoth` | Concurrent writes are all kept, as a multi-value register, until a later write resolves them. |
| `Text` | A sequence CRDT over a string. |

`(Merge, R)` on a component entity applies to the whole component. On a member entity it applies to that field,
and it **overrides the component's rule**:

```
struct Title(text: string)
Title {
  (Merge, LastWriter)
  text { (Merge, Text) }
}
```

I checked on Flecs 4.1.6-662 that a script can reopen a member entity and add a pair to it, so no fallback was
needed.

For each struct member the rule is:

1. the member's own rule;
2. otherwise the component's rule;
3. otherwise, if the member is itself a struct (not an array of them), null, meaning it merges field by field by these
   same steps;
4. otherwise the default.

A rule on a member or component covers its whole value: the value merges as one under that rule. So inside it, a
nested struct's members carry no rule (null), and rules declared on the nested type are not used there. With
`Card { (Merge, LastWriter) }` and `struct Card(pos: Vec2, ...)`, `pos` is one LastWriter register and its `x` and `y`
slots are null, even if `Vec2` declares rules of its own. Elements of arrays and vectors are covered by the rule of
the field that holds them. Use `Vec2` as a component, or give `pos` no rule and `Card` none, to merge `x` and `y`
separately.

Rules are checked against the type:

- `Text` only on `string`;
- `Counter` only on numbers;
- `AddWins` only on vectors;
- `LastWriter` and `KeepBoth` on anything.

A non-struct component's rule is in its layout record as `merge`. Members' rules are in `members`. Both are part of
the layout hash, so a rule change is a layout change.

**The default is `KeepBoth`.** It is the only rule that never silently drops a concurrent write. `LastWriter` is
cheaper but loses data whenever two actors write at once, and a wrong guess there is invisible. With `KeepBoth`, the
worst outcome of a missing annotation is a visible conflict.

Tags and pairs without values merge as `AddWins`.

### Merge states

`merge_state(rule, a, b)` joins two states of one field; `merge_fields(rules, a, b)` joins two structs' member states
in declaration order. States are DAG-CBOR, read strictly (exact keys, lists sorted strictly by key, as the merge
crate's `validate` requires). A stamp is `[wall, counter, actor]` (unsigned integers, compared in that order):

| Rule | State |
|---|---|
| `LastWriter` | `{"stamp": stamp, "value": uint}` |
| `AddWins`, `KeepBoth` | `{"entries": [[stamp, uint], ...], "removed": [stamp, ...]}` |
| `Counter` | `{"inc": [[actor, uint], ...], "dec": [[actor, uint], ...]}` |

Values are unsigned integers: the merge crate is proven over them, and the caller maps a field value to one (an index
into the values it holds, for example). The actor is a 64-bit integer in the merge crate, while change blocks name
actors by 32-byte key; the mapping between them is open.

## Sync modes

`(Sync, Mode)` on a component or tag type says how its values are kept. `Sync` is exclusive; the mode is in the
layout record (`"sync"`) and so in the layout hash.

| Mode | In the state hash | Change history | Stored |
|---|---|---|---|
| `Synced` (the default) | yes | yes | yes |
| `LatestOnly` | yes | no: only the current value is kept, older values are not retained | yes |
| `Ephemeral` | no | no | no |

- **Synced** values are in entity blocks and in every change that touches them.
- **LatestOnly** values are in entity blocks, so the root CID covers them and peers with the same state agree on it.
  A change carries the new value but a peer need not keep the old ones, and history compaction may drop them. Use it
  for values whose past is not worth keeping (a view count, a last-seen time).
- **Ephemeral** values never enter a document, a block or a hash, and neither do their layouts. A pair is Ephemeral
  when its relationship or its data type is. `World::ephemeral(roots)` returns them as one DAG-CBOR message,
  `{"version", "layouts", "entities": [{"id", "path", "tags", "pairs", "components"}]}`, listing only entities that
  have any, for a side channel such as presence or cursors. Removing or changing an Ephemeral value changes no CID.

The hierarchy is a move-safe tree CRDT, following Kleppmann et al. 2021 ("A highly-available move operation for
replicated trees"). Concurrent moves are ordered by (HLC, actor). A move that would create a cycle is skipped, and
ops are undone and redone when an earlier one arrives.

### Engine

I compared:

- Loro 1.17.1 (MIT, released 2026-10-22);
- Automerge 3.5 / crate 0.12.0 (MIT);
- yrs 0.28;
- diamond-types;
- cola.

| | Loro | Automerge 3 | yrs |
|---|---|---|---|
| Move-safe tree | yes (Kleppmann) | no | no |
| Text | Fugue, rich text | RGA family, marks | YATA |
| Counter | yes | yes | no |
| Multi-value register | no | yes (`get_all`) | no |
| Change identity | (peer, counter) | SHA-256 change hash | (client, clock) |
| wasm (npm: raw / gzip) | 3.60 / 1.19 MB | 3.64 / 1.14 MB | ~0.9 MB (old benchmark) |

diamond-types and cola are text-only, and neither has had a release since 2022/2023. I found no movable-tree
newcomer in 2026.

**Recommendation: Loro.** It is the only maintained engine with a move-safe tree, and it covers `Text`, `Counter`,
`LastWriter` (Lamport and peer order) and the tree. We would build `AddWins` and `KeepBoth` on top.

Loro's change ids are not content hashes. That avoids two competing hash graphs: our CIDs stay the only content
address, and we map each CID to Loro (peer, counter) spans. The risks:

- integrity and dedupe rest entirely on our DAG;
- Loro orders by Lamport time, not HLC;
- a reused peer id corrupts a document.

Fall back to Automerge if hash-linked changes or a native multi-value register matter more than the tree. Sources:
github.com/loro-dev/loro (`state/tree_state.rs`), loro.dev/docs/performance, automerge.org/blog/automerge-3,
github.com/automerge/automerge (`storage/chunk.rs`), github.com/y-crdt/y-crdt.

### History

The history is a Merkle DAG of change blocks (below). The world snapshot is derived from it: replaying changes in a
causal order gives the same root CID on every peer that has the same changes. To sync, peers exchange the blocks the
other side is missing, by walking from heads, as in IPFS bitswap or Automerge sync. Merging two histories means
taking the union of their change sets. The new heads are the changes nobody points to yet.

### Open questions

- **History compaction.** Can a peer drop old changes once every known peer has them? That needs a stable snapshot to
  restart from, and a rule for what a late peer with older changes does.
- **Tombstones.** How long does a deleted entity's id stay reserved? Forever is safe but grows without bound.
- **Merging across layouts.** Two changes to the same component under different layout hashes do not merge. The
  peer that knows only one layout refuses the other. A peer that knows both applies an explicit migration first.
  Nothing is merged field by field across layouts by name.
- **Clock.** HLC skew bounds, and whether a peer refuses changes too far in the future.

## History

**Reserved.** The format is fixed by `dag::Change`. Nothing signs, checks or applies changes yet.

    {"version": 1, "parents": [CID], "actor": bytes32, "hlc": uint, "state": CID,
     "grants": [CID], "signature": bytes64 | null}

- **`actor`** is the author's raw 32-byte ed25519 public key. Keyhive identifies principals the same way: the key
  itself, checked to be canonical and not of small order, rather than a hash of it. The same bytes are the HLC
  tie-break, compared bytewise.
- **`hlc`** is wall milliseconds << 16 | counter.
- **`state`** is the world root after the change.
- **`grants`** are hints: links to the changes holding the grants the author relies on. They speed up checking but
  are not trusted.
- **`signature`** is ed25519 (`verify_strict`) over `"flecs-change/1" 0x00 || DAG-CBOR of the block with signature
  null`. The domain tag means a signature over one kind of block never verifies as another. This copies Keyhive's
  envelope pattern: domain tag, version, issuer inside the signed bytes, canonical re-encoding. Keyhive's own
  envelope does not fit here, because it hashes with BLAKE3, uses its own codec and has a closed set of kinds.
- **The CID** covers the signature.

## Permissions

**Designed, reserved, not enforced.** The model follows Keyhive (github.com/inkandswitch/keyhive, `design/` and
`design/keyline/`). The vocabulary is in the `sync` module, so permissions are ordinary data in the world: they are
snapshotted, diffed and merged like everything else.

### Write access

The levels are Keyhive's: Relay < Read < Edit < Admin. Each implies the ones below it.

| Level | Relationship | Meaning |
|---|---|---|
| Relay | `(CanRelay, subtree)` | May store and forward the subtree's ciphertext blocks. |
| Read | `(CanRead, subtree)` | Holds the subtree's group key. |
| Edit | `(CanWrite, subtree)` | May write it. |
| Admin | `(CanAdmin, subtree)` | May change who has access to it. |

A grant covers the target entity and everything under it in ChildOf.

Roles are prefabs, and actors are entities with an `Actor` component holding the raw public key in hex:

```
prefab Editor { (CanWrite, site.rfcs) }
people {
  alice : Editor { Actor: {key: "3d4017c3..."} }
}
```

Delegation is transitive and attenuating, as in Keyline. Along one route an actor gets the smallest level on the
route; across routes, the largest.

A component type can narrow who may write it with `(WritableBy, role)`. For example,
`Status { (WritableBy, Admin) }` means that holding Edit on a subtree is not enough to change a `Status` in it. The
narrowing is in the layout record and the layout hash.

**The check** is a pure, deterministic function:

    allowed(actor, change, permissions at change.parents) -> allow | deny

"Permissions at change.parents" means the permission entities of the world state reached by applying the change's
causal past. It depends on nothing else: no clock, no arrival order. Changing permission data under a subtree needs
Admin on it. Every peer runs the check and drops denied changes, so all peers that hold the same blocks converge. A
Lean proof of the function is planned in a separate part of this repository.

**Concurrent revocation (Keyhive's rule, not Matrix's).** A change by X is whited out if a revocation of X's access
exists that does not have the change in its causal past. Whiting out means the change is admitted and kept, but not
materialised. This applies once that revocation is known, and it covers:

- a genuine edit made at the same time as the revocation;
- an edit back-dated by pointing at old parents.

Telling those two apart is impossible, as Keyhive's threat model notes (T5). Validity is still a deterministic
function of the block set.

Two Admins who revoke each other concurrently both lose access, as on Keyhive main. Recovery means a third Admin, or
the root, re-granting access.

Matrix instead orders conflicting power events by sender power level, then timestamp, then event id (state
resolution v2), so the more senior ban wins. That needs a total order over power events. The Keyline-style union set
evaluated over a change's past needs none, which is why I chose it. The permission pairs themselves merge as
`AddWins`. A re-grant concurrent with a revocation is a new grant, and it survives, as in Keyhive.

### Read access

There are two layers:

1. **Sync filtering.** A peer is sent only the blocks it may read, or ciphertext if it holds Relay. Keyhive and its
   sync layer filter per document; here the unit is the subtree.
2. **Encryption.** A subtree with its own read grants is an encryption boundary. Its entity blocks are encrypted with
   the subtree's group key, agreed with BeeKEM (Keyhive's TreeKEM variant: X25519 and XChaCha20-Poly1305, with
   concurrent updates kept as conflict keys). Readers without the key see only ciphertext blocks.

An encrypted block is a DAG-CBOR block:

    {"version": 1, "sealed": {"epoch": CID, "nonce": bytes24, "ciphertext": bytes}}

`epoch` links to the key epoch. **Its CID is computed over the ciphertext, never the plaintext.** A hash of plaintext
lets anyone confirm a guess at the content, and entity blocks are small and guessable ("Status: Done"). Keyhive keeps
a plaintext-side content ref (for example, an Automerge hash) outside the encryption. This design deliberately does
not.

Encryption has to be deterministic, or two peers holding the same state would get different CIDs and different
roots. So the nonce is synthetic: a keyed hash of the plaintext under the epoch key, SIV-style. This leaks equality
of identical blocks within one epoch, and only to someone who already holds both ciphertexts.

**Granularity is the subtree, not the field.** Each block is one entity, so a key per subtree maps one key to a set
of whole blocks. A key per field would need:

- per-field ciphertexts inside one block;
- key material in every record;
- layouts that reveal which fields are hidden.

It would also break diff and merge, which work per entity. Fields that need different readers belong in different
entities, for example a child entity under a separately granted subtree.

**Writers read.** Edit implies Read, because the writer must encrypt under the subtree's key, and a writer that
cannot read cannot merge.

**Revocation of read access works only going forward.** Revoking rotates the subtree's key (a BeeKEM update that
blanks the removed member's path). New blocks use the new key. Anything the revoked reader already saw or stored
stays readable to them. This cannot be undone, and the format does not pretend otherwise.

**What still leaks to a peer without the key:**

- the tree shape;
- the segment name of the boundary entity, in its readable parent;
- block counts and sizes;
- timing;
- which actors write.

Padding block sizes is an option, not planned.

### Validation

A component type or a subtree root can carry `(ValidatedBy, validator)`. The validator is an entity with
`Validator: {name, version}` that names a pure, deterministic predicate:

    valid(change, state at change.parents) -> bool

Examples: "a position moves at most N units per tick", "only into loaded chunks". Every peer runs it and drops
invalid changes, as it drops denied writes, so peers still converge.

The validator reference is in the layout record and its hash, like a merge rule. A peer that does not have the named
validator at that version refuses the layout, as it would any unknown layout. Validators are code identified by
(name, version), and a new version is a new layout.

Keyhive does not cover this. It authorises who may write, not whether the content is legal.

### Authority

Some checks cannot be pure, because they need hidden state, real time or randomness. For a subtree root with
`(Authority, actor)`, each change touching that subtree needs a countersignature from that actor before peers accept
it. **Reserved** format, `dag::Countersignature`:

    {"version": 1, "countersigns": CID, "actor": bytes32, "signature": bytes64 | null}

The signature is over `"flecs-countersign/1" 0x00 || change CID bytes`. It is another block in the DAG.

Until the countersignature arrives, a peer may show the change as tentative and roll it back if the authority
refuses. This is client-side prediction with server reconciliation, as in game netcode:

- Gabriel Gambetta, "Fast-Paced Multiplayer";
- Valve, "Source Multiplayer Networking";
- GGPO rollback.

Matrix's soft-fail is the same shape: kept in the DAG, not shown.

The trade-off is that authority mode brings back a central party and its latency for those subtrees. Pure validators
keep everything peer-to-peer. Keyhive has neither validators nor countersignatures, and it notes that trusted time
needs an outside service.

### Open questions

- The root of authority: who is Admin of a new world? A throwaway key that signs the first grant, as Keyhive does for
  groups, or the author of the first change?
- How do grants made in the world map onto Keyline certificates, if we adopt the crate rather than its semantics?
  The `keyline` crates are no_std and checked on wasm32-unknown-unknown, but they are only on git.
- BeeKEM's known gaps (Keyhive `threat_model.md` G1 to G4), for example a revoked member re-adding a key.
- How does an authority's refusal propagate: an explicit "rejected" block, or silence until a timeout?
- Do validators run on Lean-extracted code, Rust, or both, with a differential check?
