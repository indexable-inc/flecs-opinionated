//! The two output forms (SPEC.md, "Document", "Blocks"), CARv1 files, a block store, diffs between roots, and the
//! change block format (SPEC.md, "History").

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cid::Cid;
use cid::multihash::Multihash;
use ipld_core::ipld::Ipld;
use sha2::{Digest, Sha256};

use crate::check::{self, Form};
use crate::mst;
use crate::walk::{MAX_CHILDREN, Snapshot, encode};

/// The format version every document, root block and change block carries.
pub const VERSION: u64 = 2;

/// Multicodec codes: DAG-CBOR blocks, and raw blocks (blob chunks).
pub const DAG_CBOR: u64 = 0x71;
pub const RAW: u64 = 0x55;
const SHA2_256: u64 = 0x12;

/// The CIDv1 (sha2-256) of a block with the given codec.
pub fn cid_with(codec: u64, bytes: &[u8]) -> Cid {
    let mh = Multihash::<64>::wrap(SHA2_256, &Sha256::digest(bytes)).expect("32-byte digest fits");
    Cid::new_v1(codec, mh)
}

/// The CIDv1 (dag-cbor, sha2-256) of a block.
pub fn cid_of(bytes: &[u8]) -> Cid {
    cid_with(DAG_CBOR, bytes)
}

fn key(k: &str) -> String {
    k.to_string()
}

pub fn document(snap: Snapshot) -> Ipld {
    let entities = snap
        .items
        .into_iter()
        .map(|it| {
            let mut rec = it.record;
            rec.insert(key("path"), Ipld::String(it.path));
            rec.insert(
                key("parent"),
                it.parent.map_or(Ipld::Null, |p| Ipld::Integer(p as i128)),
            );
            Ipld::Map(rec)
        })
        .collect();
    let mut doc = BTreeMap::new();
    doc.insert(key("version"), Ipld::Integer(VERSION as i128));
    doc.insert(key("layouts"), Ipld::Map(snap.layouts));
    doc.insert(key("entities"), Ipld::List(entities));
    Ipld::Map(doc)
}

/// The Ephemeral parts under `roots` (SPEC.md, "Sync modes"): what a side channel sends, never a block.
pub fn ephemeral(snap: Snapshot) -> Ipld {
    let entities = snap
        .items
        .into_iter()
        .filter_map(|it| {
            let Ipld::Map(mut m) = it.ephemeral? else {
                return None;
            };
            m.insert(key("id"), it.record["id"].clone());
            m.insert(key("path"), Ipld::String(it.path));
            Some(Ipld::Map(m))
        })
        .collect();
    let mut doc = BTreeMap::new();
    doc.insert(key("version"), Ipld::Integer(VERSION as i128));
    doc.insert(key("layouts"), Ipld::Map(snap.ephemeral_layouts));
    doc.insert(key("entities"), Ipld::List(entities));
    Ipld::Map(doc)
}

/// A root CID and the blocks under it: the root block first, then entity blocks depth first, then blob blocks (SPEC.md,
/// "Blobs"), each once.
pub struct Blocks {
    pub root: Cid,
    pub blocks: Vec<(Cid, Vec<u8>)>,
}

pub fn blocks(snap: Snapshot) -> Blocks {
    let n = snap.items.len();
    let mut cids: Vec<Option<Cid>> = vec![None; n];
    let mut bytes: Vec<Vec<u8>> = vec![Vec::new(); n];
    let mut tree_blocks = Vec::new();
    // Items are in pre-order, so children come after their parent: hash in reverse.
    for i in (0..n).rev() {
        let it = &snap.items[i];
        let children = if it.children.len() > MAX_CHILDREN {
            // Past the threshold the list is an MST keyed by segment (SPEC.md, "Large child sets"); the walk refused
            // an ordered parent, and segments are already in bytewise order.
            let mut entries: Vec<(Vec<u8>, Cid)> = it
                .children
                .iter()
                .map(|c| {
                    (
                        snap.items[*c].segment.clone().into_bytes(),
                        cids[*c].unwrap(),
                    )
                })
                .collect();
            entries.sort();
            let (root, nodes) = mst::build(&entries).expect("unique sorted segments");
            tree_blocks.extend(nodes);
            let mut m = BTreeMap::new();
            m.insert(key("mst"), Ipld::Link(root));
            Ipld::Map(m)
        } else {
            Ipld::List(
                it.children
                    .iter()
                    .map(|c| {
                        Ipld::List(vec![
                            Ipld::String(snap.items[*c].segment.clone()),
                            Ipld::Link(cids[*c].unwrap()),
                        ])
                    })
                    .collect(),
            )
        };
        let mut rec = it.record.clone();
        rec.insert(key("children"), children);
        let b = encode(&Ipld::Map(rec));
        cids[i] = Some(cid_of(&b));
        bytes[i] = b;
    }
    let roots = snap
        .roots
        .iter()
        .map(|r| {
            Ipld::List(vec![
                Ipld::String(snap.items[*r].path.clone()),
                Ipld::Link(cids[*r].unwrap()),
            ])
        })
        .collect();
    let mut root = BTreeMap::new();
    root.insert(key("version"), Ipld::Integer(VERSION as i128));
    root.insert(key("layouts"), Ipld::Map(snap.layouts));
    root.insert(key("roots"), Ipld::List(roots));
    let rb = encode(&Ipld::Map(root));
    let rc = cid_of(&rb);
    let mut out = vec![(rc, rb)];
    let mut seen = BTreeSet::new();
    for (c, b) in cids.into_iter().zip(bytes) {
        let c = c.unwrap();
        if seen.insert(c) {
            out.push((c, b));
        }
    }
    for (c, b) in tree_blocks.into_iter().chain(snap.blobs) {
        if seen.insert(c) {
            out.push((c, b));
        }
    }
    Blocks {
        root: rc,
        blocks: out,
    }
}

fn varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

impl Blocks {
    /// A CARv1 file: the header `{"roots": [root], "version": 1}`, then each block as varint length, CID, bytes.
    pub fn car(&self) -> Vec<u8> {
        let mut h = BTreeMap::new();
        h.insert(key("roots"), Ipld::List(vec![Ipld::Link(self.root)]));
        h.insert(key("version"), Ipld::Integer(1));
        let header = encode(&Ipld::Map(h));
        let mut out = Vec::new();
        varint(header.len() as u64, &mut out);
        out.extend(header);
        for (c, b) in &self.blocks {
            let cb = c.to_bytes();
            varint((cb.len() + b.len()) as u64, &mut out);
            out.extend(cb);
            out.extend(b);
        }
        out
    }

    pub fn store(&self, s: &mut dyn BlockStore) {
        for (c, b) in &self.blocks {
            s.put(*c, b.clone());
        }
    }
}

/// Where blocks live. `get` returns the bytes whose CID is `cid`, if the store has them.
pub trait BlockStore {
    fn get(&self, cid: &Cid) -> Option<Vec<u8>>;
    fn put(&mut self, cid: Cid, bytes: Vec<u8>);
}

/// Blocks in memory. A hash map: nothing here needs the blocks in CID order.
#[derive(Default)]
pub struct MemoryStore {
    blocks: HashMap<Cid, Vec<u8>>,
}

impl BlockStore for MemoryStore {
    fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
        self.blocks.get(cid).cloned()
    }
    fn put(&mut self, cid: Cid, bytes: Vec<u8>) {
        self.blocks.insert(cid, bytes);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The entity (and everything under it) is only in the second root.
    Added,
    /// The entity (and everything under it) is only in the first root.
    Removed,
    /// The entity's own record differs (its children are reported separately).
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Difference {
    /// The entity's path, or `layouts.<type>` for a layout record.
    pub path: String,
    pub kind: Kind,
}

/// A block's bytes from the store, checked against its CID (any codec).
pub(crate) fn fetch(s: &dyn BlockStore, c: &Cid) -> Result<Vec<u8>, String> {
    let b = s.get(c).ok_or_else(|| format!("block {c} is missing"))?;
    if c.hash().code() != SHA2_256 || cid_with(c.codec(), &b) != *c {
        return Err(format!("block {c} does not hash to its CID"));
    }
    Ok(b)
}

/// A blob's bytes from the link its field holds (SPEC.md, "Blobs"), fetching each chunk from the store and checking
/// it against its CID and its listed length.
pub fn read_blob(s: &dyn BlockStore, link: &Cid) -> Result<Vec<u8>, String> {
    match link.codec() {
        RAW => fetch(s, link),
        DAG_CBOR => {
            let b = fetch(s, link)?;
            let Ipld::List(chunks) = serde_ipld_dagcbor::from_slice::<Ipld>(&b)
                .map_err(|e| format!("blob {link}: {e}"))?
            else {
                return Err(format!("blob {link}: the chunk list is not a list"));
            };
            let mut out = Vec::new();
            for c in chunks {
                let (n, cid) = match &c {
                    Ipld::List(x) => match x.as_slice() {
                        [Ipld::Integer(n), Ipld::Link(cid)] if cid.codec() == RAW => (*n, *cid),
                        _ => return Err(format!("blob {link}: a chunk is not [length, raw link]")),
                    },
                    _ => return Err(format!("blob {link}: a chunk is not [length, raw link]")),
                };
                let part = fetch(s, &cid)?;
                if part.len() as i128 != n {
                    return Err(format!(
                        "blob {link}: chunk {cid} is {} bytes, the list says {n}",
                        part.len()
                    ));
                }
                out.extend(part);
            }
            Ok(out)
        }
        c => Err(format!(
            "blob link {link} has codec {c:#x}, not raw or dag-cbor"
        )),
    }
}

fn load(s: &dyn BlockStore, c: &Cid) -> Result<BTreeMap<String, Ipld>, String> {
    if c.codec() != DAG_CBOR {
        return Err(format!("block {c} is not a DAG-CBOR block"));
    }
    let b = fetch(s, c)?;
    match serde_ipld_dagcbor::from_slice::<Ipld>(&b).map_err(|e| format!("block {c}: {e}"))? {
        Ipld::Map(m) => Ok(m),
        _ => Err(format!("block {c} is not a map")),
    }
}

fn pairs(v: Option<&Ipld>) -> Result<Vec<(String, Cid)>, String> {
    let Some(Ipld::List(l)) = v else {
        return Err("expected a list of [name, link]".into());
    };
    l.iter()
        .map(|x| match x {
            Ipld::List(p) => match (p.first(), p.get(1)) {
                (Some(Ipld::String(n)), Some(Ipld::Link(c))) => Ok((n.clone(), *c)),
                _ => Err("expected [name, link]".to_string()),
            },
            _ => Err("expected [name, link]".to_string()),
        })
        .collect()
}

/// An entity block's children, inline or from their MST.
fn child_list(s: &dyn BlockStore, v: Option<&Ipld>) -> Result<Vec<(String, Cid)>, String> {
    if let Some(Ipld::Map(m)) = v {
        let Some(Ipld::Link(root)) = m.get("mst") else {
            return Err("children is neither a list nor {\"mst\": link}".into());
        };
        return mst::read(s, root)?
            .into_iter()
            .map(|(k, c)| {
                String::from_utf8(k)
                    .map(|k| (k, c))
                    .map_err(|_| "a child segment in the MST is not UTF-8".to_string())
            })
            .collect();
    }
    pairs(v)
}

/// What differs between two roots, reading only blocks whose CIDs differ.
pub fn diff(s: &dyn BlockStore, a: &Cid, b: &Cid) -> Result<Vec<Difference>, String> {
    let mut out = Vec::new();
    if a == b {
        return Ok(out);
    }
    let (ra, rb) = (load(s, a)?, load(s, b)?);
    check::version(&ra, "root block")?;
    check::version(&rb, "root block")?;
    let la = check::layouts(ra.get("layouts"))?;
    let lb = check::layouts(rb.get("layouts"))?;
    for k in la.keys().chain(lb.keys()).collect::<BTreeSet<_>>() {
        let kind = match (la.get(k), lb.get(k)) {
            (Some(x), Some(y)) if x == y => continue,
            (Some(_), Some(_)) => Kind::Changed,
            (Some(_), None) => Kind::Removed,
            _ => Kind::Added,
        };
        out.push(Difference {
            path: format!("layouts.{k}"),
            kind,
        });
    }
    let (ca, cb) = (pairs(ra.get("roots"))?, pairs(rb.get("roots"))?);
    children(s, (la, lb), "", &ca, &cb, &mut out)?;
    Ok(out)
}

type Layouts<'a> = (&'a BTreeMap<String, Ipld>, &'a BTreeMap<String, Ipld>);

/// An entity block, checked strictly against the layouts of its root.
fn load_entity(
    s: &dyn BlockStore,
    c: &Cid,
    layouts: &BTreeMap<String, Ipld>,
    path: &str,
) -> Result<BTreeMap<String, Ipld>, String> {
    let m = load(s, c)?;
    let v = Ipld::Map(m);
    check::entity(&v, Form::Block, layouts).map_err(|e| format!("{path} (block {c}): {e}"))?;
    let Ipld::Map(m) = v else { unreachable!() };
    Ok(m)
}

fn children(
    s: &dyn BlockStore,
    layouts: Layouts,
    prefix: &str,
    a: &[(String, Cid)],
    b: &[(String, Cid)],
    out: &mut Vec<Difference>,
) -> Result<(), String> {
    let ma: BTreeMap<_, _> = a.iter().cloned().collect();
    let mb: BTreeMap<_, _> = b.iter().cloned().collect();
    for name in ma.keys().chain(mb.keys()).collect::<BTreeSet<_>>() {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match (ma.get(name), mb.get(name)) {
            (Some(x), Some(y)) if x == y => {}
            (Some(x), Some(y)) => {
                let (mut ea, mut eb) = (
                    load_entity(s, x, layouts.0, &path)?,
                    load_entity(s, y, layouts.1, &path)?,
                );
                let (ka, kb) = (
                    child_list(s, ea.get("children"))?,
                    child_list(s, eb.get("children"))?,
                );
                ea.remove("children");
                eb.remove("children");
                if ea != eb {
                    out.push(Difference {
                        path: path.clone(),
                        kind: Kind::Changed,
                    });
                }
                children(s, layouts, &path, &ka, &kb, out)?;
            }
            (Some(_), None) => out.push(Difference {
                path,
                kind: Kind::Removed,
            }),
            _ => out.push(Difference {
                path,
                kind: Kind::Added,
            }),
        }
    }
    Ok(())
}

/// A change block (SPEC.md, "History"). The format is fixed here; nothing signs, checks or merges changes yet, and
/// the fields marked reserved are carried but not enforced.
pub struct Change {
    /// The changes this one follows (empty for the first).
    pub parents: Vec<Cid>,
    /// The author: its raw 32-byte ed25519 public key, as Keyhive identifies principals.
    pub actor: [u8; 32],
    /// Hybrid logical clock: wall milliseconds << 16 | counter.
    pub hlc: u64,
    /// The world root after the change.
    pub state: Cid,
    /// Reserved: the certificate blocks (grants, revocations, key updates) the author relies on.
    pub grants: Vec<Cid>,
    /// Reserved: the ed25519 signature over [`Change::signing_bytes`] (64 bytes), or none until signing exists.
    pub signature: Option<[u8; 64]>,
}

/// Domain tags for the signed byte strings, so a signature over one kind never verifies as another.
pub const CHANGE_DOMAIN: &[u8] = b"flecs-change/1\0";
pub const COUNTERSIGN_DOMAIN: &[u8] = b"flecs-countersign/1\0";

impl Change {
    fn map(&self, signature: Ipld) -> Vec<u8> {
        let mut m = BTreeMap::new();
        m.insert(key("version"), Ipld::Integer(VERSION as i128));
        m.insert(
            key("parents"),
            Ipld::List(self.parents.iter().map(|c| Ipld::Link(*c)).collect()),
        );
        m.insert(key("actor"), Ipld::Bytes(self.actor.to_vec()));
        m.insert(key("hlc"), Ipld::Integer(self.hlc as i128));
        m.insert(key("state"), Ipld::Link(self.state));
        m.insert(
            key("grants"),
            Ipld::List(self.grants.iter().map(|c| Ipld::Link(*c)).collect()),
        );
        m.insert(key("signature"), signature);
        encode(&Ipld::Map(m))
    }

    /// What the author signs: the domain tag, then the block's DAG-CBOR with `signature` null.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = CHANGE_DOMAIN.to_vec();
        out.extend(self.map(Ipld::Null));
        out
    }

    /// The block; its CID covers the signature too.
    pub fn encode(&self) -> Vec<u8> {
        self.map(
            self.signature
                .map_or(Ipld::Null, |s| Ipld::Bytes(s.to_vec())),
        )
    }
}

/// Reserved (SPEC.md, "Authority"): an authority's acceptance of a change, as its own block in the DAG.
pub struct Countersignature {
    pub change: Cid,
    /// The authority's raw ed25519 public key.
    pub actor: [u8; 32],
    /// Over `COUNTERSIGN_DOMAIN` then the change's CID bytes.
    pub signature: Option<[u8; 64]>,
}

impl Countersignature {
    pub fn encode(&self) -> Vec<u8> {
        let mut m = BTreeMap::new();
        m.insert(key("version"), Ipld::Integer(VERSION as i128));
        m.insert(key("countersigns"), Ipld::Link(self.change));
        m.insert(key("actor"), Ipld::Bytes(self.actor.to_vec()));
        m.insert(
            key("signature"),
            self.signature
                .map_or(Ipld::Null, |s| Ipld::Bytes(s.to_vec())),
        );
        encode(&Ipld::Map(m))
    }
}
