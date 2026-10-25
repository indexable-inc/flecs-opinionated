//! The two output forms (SPEC.md, "Document", "Blocks"), CARv1 files, a block store, diffs between roots, and the
//! change block format (SPEC.md, "History").

use std::collections::{BTreeMap, BTreeSet};

use cid::Cid;
use cid::multihash::Multihash;
use ipld_core::ipld::Ipld;
use sha2::{Digest, Sha256};

use crate::walk::{Snapshot, encode};

/// The format version every document, root block and change block carries.
pub const VERSION: u64 = 1;

const DAG_CBOR: u64 = 0x71;
const SHA2_256: u64 = 0x12;

/// The CIDv1 (dag-cbor, sha2-256) of a block.
pub fn cid_of(bytes: &[u8]) -> Cid {
    let mh = Multihash::<64>::wrap(SHA2_256, &Sha256::digest(bytes)).expect("32-byte digest fits");
    Cid::new_v1(DAG_CBOR, mh)
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

/// A root CID and the blocks under it: the root block first, then entity blocks depth first, each once.
pub struct Blocks {
    pub root: Cid,
    pub blocks: Vec<(Cid, Vec<u8>)>,
}

pub fn blocks(snap: Snapshot) -> Blocks {
    let n = snap.items.len();
    let mut cids: Vec<Option<Cid>> = vec![None; n];
    let mut bytes: Vec<Vec<u8>> = vec![Vec::new(); n];
    // Items are in pre-order, so children come after their parent: hash in reverse.
    for i in (0..n).rev() {
        let it = &snap.items[i];
        let children = it
            .children
            .iter()
            .map(|c| {
                Ipld::List(vec![
                    Ipld::String(snap.items[*c].segment.clone()),
                    Ipld::Link(cids[*c].unwrap()),
                ])
            })
            .collect();
        let mut rec = it.record.clone();
        rec.insert(key("children"), Ipld::List(children));
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

#[derive(Default)]
pub struct MemoryStore {
    blocks: BTreeMap<Cid, Vec<u8>>,
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

fn load(s: &dyn BlockStore, c: &Cid) -> Result<BTreeMap<String, Ipld>, String> {
    let b = s.get(c).ok_or_else(|| format!("block {c} is missing"))?;
    if cid_of(&b) != *c {
        return Err(format!("block {c} does not hash to its CID"));
    }
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

fn check_version(m: &BTreeMap<String, Ipld>) -> Result<(), String> {
    match m.get("version") {
        Some(Ipld::Integer(v)) if *v == VERSION as i128 => Ok(()),
        Some(Ipld::Integer(v)) => Err(format!(
            "format version {v} is not supported (this reader knows {VERSION})"
        )),
        _ => Err("root block has no format version".into()),
    }
}

/// What differs between two roots, reading only blocks whose CIDs differ.
pub fn diff(s: &dyn BlockStore, a: &Cid, b: &Cid) -> Result<Vec<Difference>, String> {
    let mut out = Vec::new();
    if a == b {
        return Ok(out);
    }
    let (ra, rb) = (load(s, a)?, load(s, b)?);
    check_version(&ra)?;
    check_version(&rb)?;
    let empty = BTreeMap::new();
    let la = match ra.get("layouts") {
        Some(Ipld::Map(m)) => m,
        _ => &empty,
    };
    let lb = match rb.get("layouts") {
        Some(Ipld::Map(m)) => m,
        _ => &empty,
    };
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
    children(s, "", &ca, &cb, &mut out)?;
    Ok(out)
}

fn children(
    s: &dyn BlockStore,
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
                let (mut ea, mut eb) = (load(s, x)?, load(s, y)?);
                let (ka, kb) = (pairs(ea.get("children"))?, pairs(eb.get("children"))?);
                ea.remove("children");
                eb.remove("children");
                if ea != eb {
                    out.push(Difference {
                        path: path.clone(),
                        kind: Kind::Changed,
                    });
                }
                children(s, &path, &ka, &kb, out)?;
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
