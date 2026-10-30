# Format

Version 2. This file describes what the library writes, what a reader must check, and the parts that are designed
but not built yet (merging, history, permissions, validation). The library reads two kinds of world: a **script
world** it owns, filled by Flecs scripts (most of this file), and a **host world** an application owns and passes in
("Host worlds"), such as a game's live state. Sections marked **reserved** describe fields that are
in the format now but are not checked or acted on.

## Encoding

Every byte string the library produces is [DAG-CBOR](https://ipld.io/specs/codecs/dag-cbor/spec/). That gives the
canonical form of RFC 8949 §4.2.1 plus IPLD's stricter rules:

- map keys are strings, sorted by length first and then bytewise;
- integers use the shortest form;
- floats are always 64-bit; NaN and the infinities are not allowed, so writing one is an error that names the
  component and member. `-0.0` is written as `0.0` (the bytes of `0.0`), and a decoder refuses the bytes of `-0.0`,
  as `serde_ipld_dagcbor` does; a value read back from bytes is `0.0`;
- the only tag is 42 (a CID link);
- no indefinite lengths.

The same scripts produce the same bytes on every host. Native and wasm builds compile the same Flecs C with
`-ffp-contract=off`, so arithmetic in script expressions rounds the same way. In wasm, strtod is Rust's correctly
rounded parser. A test compares native and wasm output byte for byte.

A block's CID is CIDv1 with multihash sha2-256 (0x12). Entity, root and blob chunk-list blocks use codec dag-cbor
(0x71); blob chunks are raw (0x55).

Integer literals in scripts that do not fit their 64-bit type are refused on every host ("integer literal ... is out
of range"): the pinned Flecs (`vendor/flecs/SOURCE.md`) checks `ERANGE` from `strtoull`/`strtoll`, and the wasm libc
sets it as native libc does, so native and wasm agree. (Flecs 4.1.6 did not check, and such literals saturated.)

## Values

| Flecs type | Value |
|---|---|
| bool | bool |
| char, u8 to u64, i8 to i64, byte | integer |
| uptr, iptr | integer. The layout says `"uptr"`/`"iptr"` and the value is read at the host's pointer width but always written as a CBOR integer in the u64/i64 range, so native and wasm encode the same value the same way. Restoring a value that does not fit the host's pointer width is an error |
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
| opaque (a type serialised by callbacks, such as a Rust `String` or `Vec<T>` registered by flecs_ecs) | the value of its `as_type`, read through the type's `serialize` callback: a `String` is text, a `Vec<T>` an array. Its layout is the `as_type`'s layout. Written back through the meta cursor, which calls the type's assign callbacks |
| f16, bf16 (described types only, below) | float, widened exactly. Restoring a float that the type cannot hold exactly is an error |
| option (described) | `[]` for none, `[v]` for some: a list, so `Option<Option<T>>` and an optional null ref stay distinct |
| union (described; a Rust enum with data) | a map with exactly one key, the variant's name, whose value is the variant's payload, or null for a variant with none |
| map (described; a Rust `HashMap` or `BTreeMap`) | an array of `[key, value]`, sorted by the key's DAG-CBOR encoding, keys unique, so the order never depends on the map's iteration order |
| runtime (described) | null: the type holds only process-local state (a physics body handle, a GPU buffer, a cache), so the part is saved but its value is not; a restore leaves the default the type's constructor makes, for its system to rebuild |
| a Flecs map, anything else | an error that names the type |
| a component marked `DontFragment` | in a script world, an error: its values live outside the entity's type, where that walk does not read them yet. A host record saves it like any other part ("Host record") |

Errors name the component and the member path inside it, such as `component Card: member pos.x: ...` or
`member pts[2].x`.

Binary data uses the two `vector<u8>` types the `sync` module defines, so the choice is in the type, and so in the
layout (`"bytes"` or `"blob"`): `struct File { name = string; thumb = sync.Bytes; body = sync.Blob }` (one member per line). Use Bytes for small
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

In a host world (see "Host worlds") the host names the id component and gives every saved entity its id; the
library never derives one there, and never writes `#n` for a saved entity.

Not built yet: entities created at runtime by an actor rather than by a script. They will use
`SHA-256("flecs-entity-id/1" 0x00 || actor key || 0x00 || hlc)[0..16]`, which is unique per actor and per clock
tick.

## Layouts

Each component and pair type a snapshot uses gets one layout record, keyed by the type's path:

    {"base": path, "version": uint, "layout": L, "merge": rule | null, "sync": mode, "writable_by": ref | null,
     "validated_by": ref | null, "hash": bytes8}

`base` and `version` come from the type's path: a last segment ending in `V` and a decimal number that does not
start with `0` (`FooV2`, `ship.HullV12`) has base `Foo` (`ship.Hull`) and that version; any other path is its own
base at version 1. `FooV1` is refused, since it would be a second name for version 1 of `Foo`. Versions of a base
are the steps a migration chain walks ("Migrations").

`hash` is the first 8 bytes of SHA-256 over the DAG-CBOR of the record without `hash`. Every component value carries
the hash of its type's record, so a value can never be read with the wrong layout.

`L` is one of:

- a primitive name: `"bool"`, `"u8"` … `"f64"`, `"uptr"`, `"iptr"`, `"string"`, `"entity"`, `"id"`, `"bytes"`,
  `"blob"` and so on (an opaque type has its `as_type`'s layout);
- `{"struct": path | null, "members": [[name, L, rule | null], ...]}`;
- `{"enum": path | null, "underlying": L, "constants": [[name, int], ...]}`;
- `{"bitmask": path | null, "constants": [[name, uint], ...]}`;
- `{"array": L, "count": n}`;
- `{"vector": L}`;
- `{"option": L}`;
- `{"union": path | null, "variants": [[name, L | null], ...]}`, a variant's payload being a struct (a Rust tuple
  variant's members are named `0`, `1`, ...) or null;
- `{"map": [K, V]}`;
- `{"runtime": {"size": n, "align": n, "implicit": bool}}`: a part that is not saved, recorded by what its bytes in
  memory need (its Rust `size_of` and `align_of`; `size` a multiple of the power-of-two `align`), never by its Rust
  path, which a compiler (std moving `Arc`) or a move to another module changes while nothing in memory does.
  `implicit` is true when the derive made the part because the member's type has no description, false for one
  marked `runtime`. Since the size is the target's, a runtime part holding a pointer hashes differently on wasm32
  and on a 64-bit target. A reader also accepts the text body records written before this had (`{"runtime":
  path}`, `implicit:<type>` for an implicit part) and never writes it;
- `{"logical": path | null, "of": L, "rules": {rule: arg, ...}}`, a logical type ("Logical types").

`"f16"`, `"bf16"` and the four kinds from option to runtime come only from described types ("Described types"); a Flecs-only world
never has them. A Rust tuple or tuple struct is a struct whose members are named `0`, `1`, ...

Constants and members are listed in declaration order. Renaming a member, reordering members, changing a type,
changing a rule or changing the sync mode all change the hash. A layout record says everything needed to read a
value written with it, so a value whose type no longer exists in any world can still be read, checked and migrated
from its record alone.

`merge` is the whole value's rule for a non-struct type and null for a struct. A member's rule slot follows "Rules"
below: inside a value one rule merges whole, every rule slot is null. A reader refuses a record that breaks this.

### Described types

Flecs's reflection has no sum types, options, maps or half floats, and a Rust type's field order is not its memory
order. A host describes such types itself: a **described type** is a component (or any type a member uses) whose
type entity has `sync.Described`, a pointer to a host-process record holding

- the layout `L`, in the vocabulary above, with a struct member's rule slot holding the rule declared on that member
  or null;
- a read function from a value in memory to its value (as in "Values"), given a function that turns an entity into a
  ref;
- a write function from a checked value to memory, over a value the type's constructor made, given a function that
  turns a ref into an entity.

For a described type the library uses that layout and those functions instead of Flecs's reflection, so it can read
and write every Rust type: the record, the value and the memory come from one description. The rule slots are then
filled exactly as for a Flecs struct (a member's rule, else the type's `(Merge, rule)`, else nested structs null and
everything else the default; inside a covered value, all null), so a described type's record is the record a Flecs
struct of the same shape gets. The crate `flecs-describe` (`#[derive(Describe)]`) writes the description from the
Rust type, registers the same shape as Flecs meta (a struct with Rust's real member offsets where Flecs can express
it, an opaque type serialised from the description where it cannot) and checks any Flecs meta the type already has
against it, so a type, its Flecs meta and its layout record cannot disagree. A host's layout lock is the list of its
records' hashes.

`#[derive(Describe)]` takes, under `#[describe(...)]`: on the type `name = "Path"` and `runtime` (the whole type is
process-local: layout `{"runtime": {"size", "align", "implicit": false}}`, value null, restored as `Default`); on a field `merge = "Rule"`,
`runtime`, `with = path` (a module with `layout`, `read` and `from_value`) and `name = "member"`; on a variant
`name = "Variant"`. The logical rules ("Logical types"): on a field `range(min = a, max = b)` (literal bounds, either
left out), `unit = "path"` and `finite`; on a type `validate = path` (a `fn(&Self) -> Result<(), String>`); and, on a
`transparent` type only, `logical = "Name"`, `range`, `unit` and `finite`, which make the type a named logical type
over its field's layout. `core::num::NonZero<T>` is the logical type `NonZero<T>` over `T`, and `&'static T` for a
`T: Catalog` (a fixed set of named `'static` entries) is a string that `refers_to` the catalog. With `runtime` every Rust type can be described; the space game's 2797 component types (738
tags, 2059 with data) all are, 56 of them with a `runtime` part.

`sync.Described` holds a pointer, so it means something only in the process that set it; it is never part of a
snapshot, and a script world never has it.

### Logical types

A layout has two layers. The **physical** layer (the primitives, struct, enum, bitmask, array, vector, option,
union, map, runtime) alone decides the encoding and the Flecs meta. The **logical** layer refines a physical layout
with rules a value must also meet; it never changes the bytes. `NonZero<u32>` is physically `u32` and is saved as a
bare integer.

    {"logical": path | null, "of": L, "rules": {rule: arg, ...}}

`logical` is the logical type's name (null for rules declared on one member), `of` the physical layout (never itself
logical: nested refinements fold into one node), and `rules` a map, its keys sorted as DAG-CBOR sorts them. A node
with no name and no rules is not written: the member's layout is `of`, so a layout with no refinement is
byte-identical to what it was before logical types. The rules, each in one normal form (a reader refuses any other):

| rule | arg | on | a value must |
|---|---|---|---|
| `range` | `[min \| null, max \| null]`, integers for an integer `of`, floats for a float | numbers | lie within the bounds, inclusive |
| `finite` | null | floats | be finite (every saved float already is; the rule says so to readers and tools) |
| `unit` | the unit entity's path | numbers | nothing: it names the unit (`flecs.units.Length.Meters`) |
| `refers_to` | `[catalog path, [name, ...]]`, names sorted | strings | be one of the names |
| `validated` | `[check path, ...]`, sorted | any | pass each check (a function the host registered) |

A range bound is clipped to the physical type's range and left out (null) when it says nothing there; a range with
no bound is dropped. Bounds are kept as f64 in the world (`sync.Range`), so integer bounds past 2^53 lose precision.

Where they come from:

- **Flecs Script and Flecs data.** A logical type is an entity that is `(IsA, <primitive>)` and carries rule
  components: `Percent { (IsA, flecs.meta.u8)  sync.Range: {max: 100} }`. A struct member entity takes rules by being
  `(IsA, Percent)` or by carrying them itself (`Ratio { den { sync.Range: {min: 1} } }`); its own rules and those of
  every type it is (IsA) of merge (ranges intersect, units must agree, checks union; the nearest base's path is the
  name). A type entity's own rules wrap its whole layout. The vocabulary is in the sync module: `Range`, `Finite`,
  `(Unit, u)`, `(RefersTo, catalog)` with `Catalog` on the catalog (its children's names are the names),
  `(Validated, check)` with `Check` (the function pointer) on the check entity under `sync.checks`.
- **Described types.** The derive writes the node; registering the type makes the Flecs data the same rules would
  be: each named logical type under `sync.types` (IsA its primitive, with its rule components), each catalog with a
  child per name, each check entity, and on each member entity `(IsA, logical type)` or its own rules. The member's
  Flecs meta stays the physical type, with `EcsMemberRanges` and the `EcsMember` unit set from the rules, so REST and
  the inspector show them.

Enforcement: a save checks every value against its rules and refuses the first that breaks one, naming the member
path (`member den: 0 is below the minimum 1 (NonZero<u32>)`); a restore and `World::set` (and a host's set) check
the same, plus the `validated` checks, before anything is written, and never clamp or substitute. A value written
past a rule by Flecs Script or a REST write lands in the world (Flecs cannot refuse it) and is refused by the next
save. Checks run where a world is: the readers (TS, `check::value`) check range and catalog only.

A built-in check `sync.checks.NonZero` refuses zero; a signed `NonZero<T>` uses it (a range cannot say "not zero"),
an unsigned one is `range [1, null]`.

### Compatibility

`compare(old, new)` classifies the change between two layout records of one base:

- **Safe**: every old value is a new value as it is: a range loosened or dropped, `finite` added or dropped, a unit
  added or dropped, a catalog name added, a check dropped, an enum or bitmask constant added or renumbered, a union
  variant added, a merge rule, member rule slot, name, member order or permission changed, a runtime part's size,
  alignment or `implicit` changed (its value is always null; a host that patches values in memory checks the size
  itself). A runtime part with the same size, alignment and `implicit` is no change, whatever its Rust type; a legacy
  text body against a new one is Safe.
- **NeedsCheck**: old values read as they are but must pass the new rules: a range added or tightened, a check added,
  `refers_to` added.
- **NeedsMigration**: old values do not read as new ones: a physical change (a primitive, a kind, a member or variant
  added or removed, a variant's payload added or removed, an array's count), a unit changed, a catalog changed or a
  name removed, a constant removed, the sync mode changed.

The verdict is the worst change, with every change listed by member path (`a.b`, `x[]`, `m{key}`).
`Migrations::compatible(old, new)` adds a by-name edge for a Safe or NeedsCheck change (the restore through it checks
every value against the new rules) and refuses a NeedsMigration one, naming its changes. A host compares its saved
records with its live ones this way instead of comparing hashes.

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
  IsA. A part with the default `(OnInstantiate, Copy)` (called `Override` before Flecs renamed it) is copied into the instance, so it is owned; one with
  `DontInherit` is not reachable at all. When two bases give the same part, the first in `isa` order wins. Flecs
  instead searches depth first in the order of the bases' entity ids, which is not stable across worlds, so the two
  differ only for a part given by two bases at once; give it to one base, or own it on the instance.
- **Skipped parts**: `Prefab`, anything without `(OnInstantiate, Inherit)` on a base, and a second target of an
  exclusive relationship.

## Document

    {"version": 2, "layouts": {path: record}, "entities": [record + {"path": text, "parent": index | null}]}

Entities are in depth-first order from each root in the order the caller listed them. Children come in declared
order when the parent has `OrderedChildren`, and otherwise sorted by segment (bytewise). Flecs's own child order
without `OrderedChildren` is table and id order, which depends on the order entities were created and is not the
same across worlds, so it is not used. `World::roots()` lists the named roots (without `flecs` and `sync`), also
sorted bytewise.

Ephemeral parts (see "Sync modes") and their layouts are never in a document.

## Blocks

Each entity is one block: its record plus `"children"`, which is `[[segment, CID], ...]` in the child order above,
or `{"mst": CID}` past 4096 children ("Large child sets"). The ChildOf
tree is the Merkle tree. The world root block is:

    {"version": 2, "layouts": {path: record}, "roots": [[path, CID], ...]}

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

Up to 4096 children, the child list is inline. Its CID depends only on its contents, not on the order the children
were inserted (the sort is by segment), so it is history-independent. Past 4096 the list is `{"mst": CID}`, the root of
a Merkle Search Tree in the atproto layout, keyed by the segment's UTF-8 bytes with the child's CID as the value. A
parent with `OrderedChildren` and more than 4096 children is refused, since an MST is ordered by key.

The MST (as atproto's repository tree, with this library's keys and values):

- A key's depth is the number of leading zero bits of SHA-256(key), divided by 2 and rounded down (fanout 4).
- A node is `{"l": CID | null, "e": [{"p": uint, "k": bytes, "v": CID, "t": CID | null}, ...]}`: `l` is the subtree of
  keys below the first entry, each entry's `t` the subtree between it and the next. `p` is how many leading bytes the
  key shares with the previous entry's key in the node (0 for the first) and `k` the rest.
- All entries of a node have the same depth, and its subtrees are one depth lower. Empty nodes between two levels are
  kept so a link never skips a level; the root is the node at the highest depth that holds an entry, and an empty
  tree is one node with `"e": []`.
- Keys are strictly increasing bytewise across the whole tree.
- Readers check every node: keys in order, every key at its node's depth, `p` within the previous key, no empty node
  except the root of an empty tree or between levels.

The shape depends only on the keys, so the tree is history-independent, and one insert rewrites O(log n) blocks. No
Rust MST crate fitted (atrium-repo needs tokio, rsky-repo pulls in secp256k1 and much of a PDS, `merkle-search-tree`
is not IPLD-shaped), so `crates/flecs-snapshot/src/mst.rs` implements it.

### Incremental hashing

`Host::blocks` caches each saved entity's block and CID, and on the next call rebuilds only the entities whose Flecs
table changed: an entity moved to another table, or its table's per-column change counters (`dirty_state`, which
Flecs bumps on `set`, `modified` and every query that writes the column) moved since the last call. The MST over the
entities is then rebuilt from the cached CIDs, which costs hashing about n/4 small nodes. A write that bypasses all of
those (a raw pointer from `get_mut` with no `modified`) is not seen; `Host::blocks_full` rebuilds everything, and a
test compares the two. Script worlds always rebuild.

A changed table does not rebuild its entities' records outright. When every component of an entity is plain data
(Flecs meta of numbers, bools, entities, enums and bitmasks, and arrays and structs of those; no strings, vectors,
opaque or described types), the cache keeps the components' raw bytes, and an entity whose table and bytes are
unchanged keeps its block. Entities with a component that is not plain data, or whose bytes changed, are
re-encoded. An entity member is compared by its local id, so renaming or re-parenting the entity it points at
(which changes a `{"path"}` ref) is seen only by `blocks_full`. Two things change a record without moving or
dirtying the entity's table, so they are checked on every call: a parent's saved children and their order (a
`Parent` child, `set_child_order`), compared with the list cached for it; and DontFragment parts, whose ids and
bytes join the raw bytes above (an entity with a DontFragment part that is not plain data is re-encoded every call). `Host::blocks_new` returns only the blocks this Host has not returned since its cache
was filled, for a store that kept every earlier result.

## Versioning

- Every document, root block, change block and countersignature carries `"version"`. A reader refuses any version it
  does not know, and so does `diff`. The library never guesses.
- Every component value carries its layout hash. A reader built for certain layouts checks them (`checkLayouts` in
  TS) and refuses a mismatch instead of reading the fields wrongly.
- Readers are strict: every map in a document, root block, entity block or layout record must have exactly the keys
  this spec lists, layouts must be well formed (rule slots as in "Rules", known primitive names, hash recomputed),
  and every value must fit its layout. Anything else is refused, because an unknown key may change what the record
  means. `check::document` in Rust and `checkDocument` in TS do this; the TS reader checks the layout hashes' shape
  and leaves recomputing them to the Rust reader. Strict readers mean no key can be added within a version, except an
  optional key left out wherever its feature is unused: every earlier document is then unchanged and still reads,
  and an earlier reader refuses a document that uses it rather than misreading it. Host records' `children` and
  `parent_component` were added to version 2 this way.
- Anything else needs a new version.
- Version 2 added `base` and `version` to layout records, `uptr`, `iptr` and opaque types, described types (`f16`,
  `bf16`, option, union, map, runtime), the MST child list and host documents. Readers of this version refuse version 1; nothing stored version 1 data outside tests.
- A component's layout never changes under the same type. A changed type is a new one, `FooV2`, with a migration from
  `Foo`, and a value is only ever read with the record it was written with ("Migrations"). Nothing converts
  implicitly: a value converts only along a migration the reader was given.

## Host worlds

A host world is a Flecs world an application owns, such as a game's live state, passed to the library as an
`ecs_world_t*`. The library links the application's Flecs (crate `flecs-c` with the `host` feature depends on the
host's `flecs_ecs_sys` instead of compiling its own), so the process has one Flecs. The library does not install its
OS API, change the lookup path, run scripts or derive ids there. It adds the `sync` module (the script in "The sync
module") if the world has none, so the host can put `(Sync, Ephemeral)` and merge rules on its own types.

The host names two things:

- the **id component**, a struct of exactly `{hi: u64, lo: u64}` (the game's `Uid`), whose 16 big-endian bytes are
  the entity's stable id;
- the **select tag** (the game's `Persist`): the saved entities are the entities that have it, disabled ones too,
  never prefabs; or only those in one subtree.

Every saved entity must have an id, and two saved entities with one id are refused.

### Host record

    {"id": bytes16, "name": text | null, "parent": ref | null,
     "tags": [ref], "pairs": [...], "components": {...}, "isa": [ref],
     "children": [ref],          (only on a parent whose children Flecs keeps in order)
     "parent_component": true}   (only when the parent is held in Flecs's Parent component)

`tags`, `pairs` and `components` are as in "Entity record". `isa` lists the entity's direct bases, sorted by their
ref's DAG-CBOR. There is no `inherited`: an inherited part belongs to the prefab, which is authored, not saved. The
id component, the select tag, the name (`Identifier`), `ChildOf`, `Parent` and `(ParentDepth, n)` are not parts: the
parent is `parent` and the name is `name`. Ephemeral parts are left out.

**Hierarchy.** Flecs keeps a parent in one of two storages: the fragmenting `(ChildOf, p)` pair, or the
non-fragmenting `Parent` component (`EcsParent`, whose `value` is p; `ecs_new_w_parent`). `parent` is p in both.
`"parent_component": true` says the entity holds it in `Parent`; the key is left out for `ChildOf` and for an entity
with no parent. The storage is saved because it is the host's choice, which its queries and tables see, and a
restore recreates it.

**Child order.** Flecs keeps an order for p's children when p has `OrderedChildren`, and always once any child of p
is in `Parent` storage; the order covers p's children of both storages. Then p's record has `"children"`: its saved
children in that order, each a `{"id"}` ref. It is left out when p has no saved child or Flecs keeps no order for p,
so children without an order (`ChildOf` without `OrderedChildren`) are written as before. The order is the world's
state (`set_child_order` changes it), so it is saved; the rest of the record is still the same bytes for the same
state, whatever order the entities were made in.

The order lives on the parent, as one list, because that is how Flecs holds it: a reorder rewrites one block, the
parent's, and adding or removing a child rewrites the parent and that child. An index on each child would say the
same, but an insert at the front would renumber and rewrite every sibling's block; a sortable key on each child (a
fractional index) would not, but Flecs holds no such key, so it would not be a function of the world's state and two
worlds in one state could write different bytes. The cost: a child whose parent is not saved has no order in the
document, and a restore appends it after the parent's live children in document (id) order.

**DontFragment parts.** A tag, component or pair whose type (or relationship) has `DontFragment` lives in Flecs's
sparse storage, outside the entity's table. It is read from there and written into `tags`, `pairs` and `components`
like any other part, with its value: an exclusive one such as `(InChunk, chunk)` is one pair. Nothing in the record
says the storage; it belongs to the type, and adding the part in another world stores it the same way.

A reader also checks: `parent_component` is `true` and only with a non-null `parent`; `children` is not empty and
lists `{"id"}` refs to records of the document whose `parent` is this record, each such record exactly once; and a
record whose `parent` is a record of the document and that has `parent_component` makes that parent list
`children`.

Refs in a host record:

- a saved entity: `{"id"}`;
- an entity outside the selection with an id: `{"id"}`, only when the caller allows outside refs (`Outside::ById`);
  refused otherwise;
- anything else (prefabs, components, modules, the host's named entities): `{"path"}`, refused when a segment is
  unnamed (`#n`), since that numbering is local to one world.

Also refused, naming the entity and part: a disabled `CanToggle` component, a wildcard id, two targets of an
exclusive relationship.

### Host document

    {"version": 2, "ids": path, "layouts": {path: record}, "entities": [host record]}

`ids` is the id component's path. Entities are sorted by id.

### Host blocks

Each entity is one block, its host record. The root block is

    {"version": 2, "ids": path, "layouts": {path: record}, "entities": CID}

where `entities` is the root of an MST keyed by the 16 id bytes with each entity's block CID as the value. A change to
one entity rewrites its block and O(log n) MST nodes. The root CID is the state hash.

### Restore

`Host::restore` creates a document's entities in the world, all or nothing:

1. Check the document strictly (as "Versioning"), then migrate every value whose layout the world does not have
   (below). A layout with no migration to a live type is refused. A value whose layout the world has is checked
   against it straight from the document's bytes (a dry run of the write), with no value tree; every value is
   checked before the world changes.
2. Refuse when the world is deferred (a stage, or in a system or observer), and when an id is already held by a live entity,
   unless the caller remaps ids (`Ids::Remint`, whose function gives each saved id a fresh one; refs to saved entities
   follow the new ids).
3. Create every entity with `ecs_new` and set its id. Nothing is placed or moved; restore makes new entities.
4. Set parents (parents before children, refusing a cycle or a depth past `FLECS_DAG_DEPTH_MAX`), as `(ChildOf, p)`
   or, for `parent_component`, the `Parent` component, and names (refusing a name its parent already has). A
   document whose `children` do not match its parents is refused before this (see "Host record").
5. Add `IsA` bases (copying their `Copy` parts), then pairs, tags and components, all in one deferred batch so
   each entity moves table once; then write each value in place (from the document's bytes, or the migrated value)
   and mark it modified; then remove any part a base copied that the record does not list. DontFragment parts are
   added the same way.
6. Put each restored parent's restored children in its `children` order (`ecs_set_child_order`). Children it had
   before keep their places; the restored ones fill the places they were given, in the saved order.
7. Add the select tag last, so observers of it see whole entities.
8. Optionally read every entity back and compare it with the record (`verify`).

On any error every entity made so far is deleted and the world is as it was, except for ids Flecs recycles.

### Live migration

`Host::migrate_live` moves every value of one type to another along a migration chain, in place: each owner of
`Foo`, `(Foo, *)` or `(rel, Foo)`, prefabs included, gets `FooV2` (or `(FooV2, *)`, `(rel, FooV2)`) with the
converted value and loses `Foo`. It is all or nothing: every value is read and converted before anything is
written, an entity that already has `FooV2` is refused, and a failed write removes the `FooV2` parts it added. The
host runs it after a patch adds `FooV2`, before any system reads it.

## Migrations

A migration is data: an edge from one layout record to another, keyed by (old hash, new hash), holding both records
and a conversion. A set of edges (`Migrations`) is passed to `restore`, `migrate_live` and the readers; a chain is any
path of edges (`Foo` to `FooV2` to `FooV3`), found by breadth-first search from the old hash. When a saved record has
no live type, the target is the live type reachable from it with the highest version; two reachable live types of the
same base and version are refused. An edge's conversion is one of:

- **by name** (the default), with an optional `renamed_from` map from a new member path to the old one (`"pos.x"`):
  - a struct member with the same name (or the old name `renamed_from` gives) moves, converted by its own layouts;
  - a number widens: unsigned to a wider unsigned or signed type, signed to a wider signed type, `f32` to `f64`, an
    integer of up to 32 bits to `f64`, `uptr` and `u64` (and `iptr` and `i64`) to each other; any other numeric change,
    narrowing included, is an error;
  - an enum or bitmask converts by constant name; a constant the new type lacks is an error;
  - arrays and vectors convert element by element; an array converts to a vector, or to an array of the same count;
  - an option converts its value; a value converts to an option of its type (some); an option to its value type is
    an error for none;
  - a union converts by variant name (a variant's payload by these rules); a value in a variant the new type lacks
    is an error;
  - a map converts its keys and values, and is re-sorted; two keys that convert to one are an error;
  - `f16` and `bf16` widen to `f32` and `f64`; a runtime value stays null;
  - a member only the new layout has gets its default: the member of the value the new type's constructor makes when
    a live world has the type, zero (false, empty, null) otherwise;
  - a member only the old layout has is dropped and reported (`Report::dropped`);
  - anything else (a struct to a number, a string to an enum) is an error, never an abort;
- **custom**, a function from the old value to the new one, for anything by-name cannot express.

Edges are stored as DAG-CBOR (`Migrations::to_cbor`, `from_cbor`: `{"version": 2, "edges": [{"from": record, "to":
record, "renamed_from": {new: old}}]}`; custom edges are code and are registered at run time).

## API

The Rust API a host calls (crate `flecs-snapshot`, feature `host`; `crates/flecs-snapshot/src/host.rs`,
`migrate.rs`):

    pub struct Host { /* one world, its id component and select tag, and the block cache */ }
    impl Host {
        /// `world` lives as long as the Host. `ids` and `select` are entity paths.
        pub unsafe fn new(world: *mut ecs_world_t, ids: &str, select: &str) -> Result<Host, String>;
        pub fn document(&self, sel: &Selection, opts: &Encode) -> Result<Ipld, String>;
        pub fn encode(&self, sel: &Selection, opts: &Encode) -> Result<Vec<u8>, String>; // DAG-CBOR document
        pub fn blocks(&mut self, sel: &Selection, opts: &Encode) -> Result<Blocks, String>; // incremental
        pub fn blocks_new(&mut self, sel: &Selection, opts: &Encode) -> Result<Blocks, String>; // only unseen blocks
        pub fn blocks_full(&mut self, sel: &Selection, opts: &Encode) -> Result<Blocks, String>;
        pub fn restore(&mut self, doc: &[u8], m: &Migrations, opts: &mut Restore) -> Result<Restored, String>;
        pub fn migrate_live(&mut self, m: &Migrations, from: &str, to: &str) -> Result<Report, String>;
        pub fn layout_record(&self, ty: &str) -> Result<Ipld, String>;
    }
    pub enum Selection { Tagged, Subtree(ecs_entity_t) }
    pub struct Encode { pub outside: Outside }            // Outside::{Refuse, ById}
    pub struct Restore { pub ids: Ids, pub verify: bool } // Ids::{Keep, Remint(Box<dyn FnMut([u8; 16]) -> [u8; 16]>)}
    pub struct Restored { pub entities: Vec<([u8; 16], ecs_entity_t)>, pub report: Report }
    pub struct Report { pub dropped: Vec<String>, pub migrated: Vec<String> }
    /// What differs between two host roots, by entity id (Kind::{Added, Removed, Changed}); entity blocks are not
    /// fetched.
    pub fn diff(s: &dyn BlockStore, a: &Cid, b: &Cid) -> Result<Vec<([u8; 16], Kind)>, String>;

    // Described types (SPEC.md, "Described types").
    pub trait Refs { fn reference(&mut self, e: ecs_entity_t) -> Result<Ipld, String>; }
    pub trait Resolve { fn entity(&mut self, r: &Ipld) -> Result<ecs_entity_t, String>; }
    pub type ReadFn = unsafe fn(ptr: *const u8, refs: &mut dyn Refs) -> Result<Ipld, String>;
    pub type WriteFn = unsafe fn(v: &Ipld, ptr: *mut u8, ids: &mut dyn Resolve) -> Result<(), String>;
    pub struct Described { pub layout: Ipld, pub read: ReadFn, pub write: WriteFn }
    pub unsafe fn describe(world: *mut ecs_world_t, ty: ecs_entity_t, d: Described) -> Result<(), String>;
    pub unsafe fn described(world: *const ecs_world_t, ty: ecs_entity_t) -> Option<&'static Described>;

    pub struct Migrations { /* edges keyed by (old hash, new hash) */ }
    impl Migrations {
        pub fn by_name(&mut self, from: Ipld, to: Ipld, renamed_from: &[(&str, &str)]) -> Result<(), String>;
        pub fn custom(&mut self, from: Ipld, to: Ipld, f: Box<dyn Fn(&Ipld) -> Result<Ipld, String>>) -> Result<(), String>;
        pub fn convert(&self, from: &[u8; 8], to: &[u8; 8], v: &Ipld, report: &mut Report) -> Result<Ipld, String>;
        /// A by-name edge for a Safe or NeedsCheck change; an error naming the changes otherwise.
        pub fn compatible(&mut self, from: Ipld, to: Ipld) -> Result<Verdict, String>;
        pub fn to_cbor(&self) -> Vec<u8>;
        pub fn from_cbor(b: &[u8]) -> Result<Migrations, String>;
    }

    // Logical types (SPEC.md, "Logical types", "Compatibility").
    pub enum Compat { Safe, NeedsCheck, NeedsMigration }
    pub struct Verdict { pub compat: Compat, pub changes: Vec<compat::Change> } // Change { path, compat, what }
    pub fn compare(old: &Ipld, new: &Ipld) -> Result<Verdict, String>;
    pub type CheckFn = fn(&Ipld) -> Result<(), String>;
    /// Makes (or finds) the check entity at `path` (by convention under `sync.checks`) and sets its function.
    pub unsafe fn register_check(world: *mut ecs_world_t, path: &str, f: CheckFn) -> Result<ecs_entity_t, String>;

The crate `flecs-describe` (features `flecs_ecs`, `glam`, `glamx`, `half`, `bitflags`):

    pub trait Describe: Sized + 'static {
        fn type_name() -> String;
        fn layout() -> Ipld;
        fn read(&self, refs: &mut dyn Refs) -> Result<Ipld, String>;
        fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> Result<Self, String>;
        fn flecs() -> Flecs; // what Flecs's reflection can hold; Flecs::Opaque by default
    }
    #[derive(Describe)]
    /// Describes `component` as `T` and registers (or checks) its Flecs meta.
    pub unsafe fn register<T: Describe>(world: *mut ecs_world_t, component: ecs_entity_t) -> Result<(), String>;
    /// Makes a component at `path` with Rust's lifecycle hooks, then registers `T` on it.
    pub unsafe fn component<T: Describe + Default + Clone>(world: *mut ecs_world_t, path: &str)
        -> Result<ecs_entity_t, String>;
    /// A fixed set of named `'static` entries; `&'static T` is saved as the entry's name.
    pub trait Catalog: 'static {
        const CATALOG: &'static str;
        fn entries() -> &'static [&'static Self];
        fn name(&self) -> &str;
    }

Every error is a `String` naming the entity (by id), the part and the member path. Nothing panics or aborts on bad
input: the host's Flecs build may not have soft asserts, so the library checks each argument before calling Flecs.

## The sync module

Every world starts with a `sync` module, defined by a Flecs script (`crates/flecs-snapshot/src/schema.rs`) and put
on the lookup path, so scripts write `(Merge, Text)` without the prefix. It holds:

- the `Id` component;
- the `Merge` relationship and its rules;
- the `Sync` relationship and its modes;
- the `Bytes` and `Blob` types;
- the `Described` component (a host-process pointer to a type's description; "Described types");
- the logical vocabulary ("Logical types"): `Range`, `Finite`, `Unit`, `RefersTo`, `Catalog`, `Validated`, `Check`,
  the scope `checks` (with the built-in `NonZero`) and the scope `types` (the logical types described types make);
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
`Card { (Merge, LastWriter) }` and `struct Card { pos = Vec2 ... }`, `pos` is one LastWriter register and its `x` and `y`
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
- **Merging across layouts.** Two changes to the same component under different layout hashes do not merge
  directly. A peer that knows only one layout refuses the other. A peer that has a migration between them
  ("Migrations", by name or custom) converts the older value first, then merges under one layout; the migration is
  data both peers can hold, so they convert the same way. Open: whether a merge result must be stored under the
  newer layout only.
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
