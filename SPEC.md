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
  component;
- the only tag is 42 (a CID link);
- no indefinite lengths.

The same scripts produce the same bytes on every host. Native and wasm builds compile the same Flecs C with
`-ffp-contract=off`, so arithmetic in script expressions rounds the same way. In wasm, strtod is Rust's correctly
rounded parser. A test compares native and wasm output byte for byte.

A block's CID is CIDv1, codec dag-cbor (0x71), multihash sha2-256 (0x12).

## Values

| Flecs type | Value |
|---|---|
| bool | bool |
| char, u8 to u64, i8 to i64, uptr, iptr, byte | integer |
| f32, f64 | float (an f32 is widened to f64 exactly) |
| string | text, or null for a null pointer; it must be UTF-8 |
| entity | a ref, or null for 0 |
| id | a ref, `[ref, ref]` for a pair, or null |
| enum | the constant's name; a value that is not a constant is an error |
| bitmask | an array of constant names; leftover bits are an error |
| struct | a map from member name to value |
| array, or an inline member array (`count > 1`) | an array |
| vector | an array |
| opaque, map, anything else | an error that names the type |

A **ref** is `{"id": bytes16}` for an entity with a stable id, or `{"path": text}` for one without (Flecs's
builtins, such as `flecs.core.Prefab`). Snapshots never contain Flecs entity ids.

Paths are Flecs paths. Each segment is the entity's name with `\` and `.` escaped by `\`. An unnamed child is
`#n`, where n counts the parent's unnamed children in Flecs id order. That ordering is local to one world, so
unnamed entities are not history-independent; name anything that will be compared across worlds.

## Stable ids

Every entity outside `flecs` and `sync` gets a 16-byte id the first time a script run sees it:

    id = SHA-256("flecs-entity-id/1" 0x00 || script name || 0x00 || path)[0..16]

`path` is the entity's path at that moment. The id is stored in the `sync.Id` component and never recomputed, so
renaming or moving an entity keeps its id. Two peers that run the same script under the same name give its entities
the same ids, which is what lets them merge the entity instead of duplicating it. `sync.Id` has
`(OnInstantiate, DontInherit)`: a prefab instance gets its own id from its own path rather than the prefab's.

Pair targets, entity-typed fields and `isa` lists refer to entities by this id.

Not built yet: entities created at runtime by an actor rather than by a script. They will use
`SHA-256("flecs-entity-id/1" 0x00 || actor key || 0x00 || hlc)[0..16]`, which is unique per actor and per clock
tick.

## Layouts

Each component and pair type a snapshot uses gets one layout record, keyed by the type's path:

    {"layout": L, "merge": rule | null, "writable_by": ref | null, "validated_by": ref | null, "hash": bytes8}

`hash` is the first 8 bytes of SHA-256 over the DAG-CBOR of the record without `hash`. Every component value carries
the hash of its type's record, so a value can never be read with the wrong layout.

`L` is one of:

- a primitive name: `"bool"`, `"u8"` … `"f64"`, `"string"`, `"entity"`, `"id"` and so on;
- `{"struct": path | null, "members": [[name, L, rule | null], ...]}`;
- `{"enum": path | null, "underlying": L, "constants": [[name, int], ...]}`;
- `{"bitmask": path | null, "constants": [[name, uint], ...]}`;
- `{"array": L, "count": n}`;
- `{"vector": L}`.

Constants and members are listed in declaration order. Renaming a member, reordering members, changing a type or
changing a rule all change the hash.

## Entity record

    {"id": bytes16 | null, "name": text | null,
     "tags": [ref], "pairs": [[rel, tgt] | [rel, tgt, {"type": path, "hash": bytes8, "value": v}]],
     "components": {type path: {"hash": bytes8, "value": v}},
     "isa": [ref],
     "inherited": {"tags": [ref], "components": [path], "pairs": [[rel, tgt]]}}

- **tags and pairs** are sorted by their DAG-CBOR encoding. The record leaves out the name, the parent, `IsA`, the
  stable id and ids that carry flags.
- **Builtin components** (anything under `flecs`) are written as tags with no value. Their layouts depend on the
  pointer size, so leaving the values out keeps wasm32 and 64-bit output identical.
- **`isa`** lists every prefab the entity is an instance of, nearest first. Prefabs at the same depth are in path
  order.
- **Inherited parts** are the tags, components and pairs reached through `isa` and not owned by the entity. A
  component with Flecs's default `(OnInstantiate, Override)` is copied into the instance, so it counts as owned. Only
  `(OnInstantiate, Inherit)` components show up under `inherited`.
- **Skipped parts**: `Prefab`, anything with `DontInherit`, and a second target of an exclusive relationship.

## Document

    {"version": 1, "layouts": {path: record}, "entities": [record + {"path": text, "parent": index | null}]}

Entities are in depth-first order from each root in the order the caller listed them. Children come in declared
order when the parent has `OrderedChildren`, and otherwise sorted by segment.

## Blocks

Each entity is one block: its record plus `"children": [[segment, CID], ...]`, in the child order above. The ChildOf
tree is the Merkle tree. The world root block is:

    {"version": 1, "layouts": {path: record}, "roots": [[path, CID], ...]}

The library writes a block set as a CARv1 file: the header `{"roots": [root CID], "version": 1}`, then the root block,
then the entity blocks depth first, each written once.

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
- Within a version, changes are additive only: new optional keys in records. A reader ignores unknown keys only
  where this spec allows it, which today means only the top level of the document and root block. Unknown keys
  inside an entity record or a layout are a reason to refuse it, because they may change what the record means.
- Anything else needs a new version.
- Migrations are explicit code, written for one (old layout hash → new layout hash) pair. Nothing converts
  implicitly.

## The sync module

Every world starts with a `sync` module, defined by a Flecs script (`crates/flecs-snapshot/src/schema.rs`) and put
on the lookup path, so scripts write `(Merge, Text)` without the prefix. It holds:

- the `Id` component;
- the `Merge` relationship and its rules;
- the reserved permission and validation vocabulary below.

## Merging

Merging is designed here and not implemented in this crate. The rules are data in the world, and the code reads them
as runtime values: `merge::Rule` is an enum read from the world, never a type parameter. A separate crate in this
repository implements the merge functions themselves, behind one non-generic `merge(rule, a, b)`. This crate calls
it through the `Merger` trait.

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
3. otherwise, if the member is itself a struct, null, meaning it merges field by field by these same steps;
4. otherwise the default.

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
