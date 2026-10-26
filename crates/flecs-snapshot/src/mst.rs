//! Merkle Search Trees in the atproto layout (SPEC.md, "Large child sets"): a history-independent sorted map from byte
//! keys to CIDs, stored as DAG-CBOR nodes. Used for child lists past 4096 entries and for a host world's entities.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

use cid::Cid;
use ipld_core::ipld::Ipld;
use sha2::{Digest, Sha256};

use crate::dag::{BlockStore, DAG_CBOR, cid_of, fetch};
use crate::walk::encode;

pub type R<T> = Result<T, String>;

/// A key's depth: the leading zero bits of SHA-256(key), two bits per level (fanout 4).
pub fn depth(key: &[u8]) -> u32 {
    let h = Sha256::digest(key);
    let mut zeros = 0;
    for b in h {
        if b == 0 {
            zeros += 8;
            continue;
        }
        zeros += b.leading_zeros();
        break;
    }
    zeros / 2
}

fn link(c: Option<Cid>) -> Ipld {
    c.map_or(Ipld::Null, Ipld::Link)
}

fn common(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// The tree over `entries` (strictly increasing keys, each with its depth): the root CID and every node block, root
/// first. Keys that are not strictly increasing are an error.
/// Node blocks: (CID, bytes).
pub type Nodes = Vec<(Cid, Vec<u8>)>;

pub fn build(entries: &[(Vec<u8>, Cid)]) -> R<(Cid, Nodes)> {
    for w in entries.windows(2) {
        if w[0].0 >= w[1].0 {
            return Err("MST keys are not strictly increasing".into());
        }
    }
    let keyed: Vec<(&[u8], Cid, u32)> = entries
        .iter()
        .map(|(k, v)| (k.as_slice(), *v, depth(k)))
        .collect();
    let mut blocks = Vec::new();
    let top = keyed.iter().map(|e| e.2).max();
    let root = match top {
        None => {
            let b = node(None, &[]);
            let c = cid_of(&b);
            blocks.push((c, b));
            c
        }
        Some(level) => level_node(&keyed, level, &mut blocks),
    };
    // Built bottom-up, so the root came last.
    blocks.reverse();
    Ok((root, blocks))
}

fn node(l: Option<Cid>, e: &[Ipld]) -> Vec<u8> {
    let mut m = BTreeMap::new();
    m.insert("l".to_string(), link(l));
    m.insert("e".to_string(), Ipld::List(e.to_vec()));
    encode(&Ipld::Map(m))
}

/// The node at `level` over `keys` (every key at `level` or below): its entries are the keys at `level`, and each run
/// of lower keys between them is a subtree one level down. A run with no key at that next level still gets a node
/// there (no entries, only `l`), so links never skip a level.
fn level_node(keys: &[(&[u8], Cid, u32)], level: u32, out: &mut Vec<(Cid, Vec<u8>)>) -> Cid {
    let sub = |run: &[(&[u8], Cid, u32)], out: &mut Vec<(Cid, Vec<u8>)>| -> Option<Cid> {
        if run.is_empty() {
            None
        } else {
            Some(level_node(run, level - 1, out))
        }
    };
    let mut entries = Vec::new();
    let mut start = 0;
    let mut l = None;
    let mut prev: &[u8] = &[];
    let mut first = true;
    let at: Vec<usize> = (0..keys.len()).filter(|i| keys[*i].2 == level).collect();
    for (n, &i) in at.iter().enumerate() {
        let left = sub(&keys[start..i], out);
        if n == 0 {
            l = left;
        } else if let Some(Ipld::Map(m)) = entries.last_mut() {
            m.insert("t".to_string(), link(left));
        }
        let (k, v, _) = keys[i];
        let p = if first { 0 } else { common(prev, k) };
        first = false;
        let mut m = BTreeMap::new();
        m.insert("p".to_string(), Ipld::Integer(p as i128));
        m.insert("k".to_string(), Ipld::Bytes(k[p..].to_vec()));
        m.insert("v".to_string(), Ipld::Link(v));
        m.insert("t".to_string(), Ipld::Null);
        entries.push(Ipld::Map(m));
        prev = k;
        start = i + 1;
    }
    let tail = sub(&keys[start..], out);
    if at.is_empty() {
        l = tail;
    } else if let Some(Ipld::Map(m)) = entries.last_mut() {
        m.insert("t".to_string(), link(tail));
    }
    let b = node(l, &entries);
    let c = cid_of(&b);
    out.push((c, b));
    c
}

/// A node's subtrees: (fingerprint, CID).
type Kids = Vec<(u128, Cid)>;

/// What a previous build made, so the next build over mostly the same entries re-encodes only the nodes whose runs
/// changed (SPEC.md, "Incremental hashing"). A run is fingerprinted by 128 bits of SipHash over its level, keys and
/// values; a node whose fingerprint was seen is reused with its CID and bytes.
#[derive(Default)]
pub struct Memo {
    runs: HashMap<u128, Cid>,
    /// A node's bytes and its subtrees' (fingerprint, CID).
    nodes: HashMap<Cid, (Vec<u8>, Kids)>,
    depths: HashMap<Vec<u8>, u32>,
}

fn fingerprint(keys: &[(&[u8], Cid, u32)], level: u32) -> u128 {
    let mut a = std::hash::DefaultHasher::new();
    let mut b = std::hash::DefaultHasher::new();
    0xa5u8.hash(&mut b);
    for h in [&mut a, &mut b] {
        level.hash(h);
        keys.len().hash(h);
        for (k, v, _) in keys {
            k.hash(h);
            Hash::hash(v, h);
        }
    }
    (u128::from(a.finish()) << 64) | u128::from(b.finish())
}

/// As `build`, reusing `memo` (and refilling it with this tree's nodes). With `only_new`, the blocks returned are the
/// nodes `memo` did not already hold; otherwise every node of the tree, root first.
pub fn build_memo(entries: &[(Vec<u8>, Cid)], memo: &mut Memo, only_new: bool) -> R<(Cid, Nodes)> {
    for w in entries.windows(2) {
        if w[0].0 >= w[1].0 {
            return Err("MST keys are not strictly increasing".into());
        }
    }
    let mut old = std::mem::take(memo);
    let keyed: Vec<(&[u8], Cid, u32)> = entries
        .iter()
        .map(|(k, v)| {
            let d = match old.depths.remove(k.as_slice()) {
                Some(d) => d,
                None => depth(k),
            };
            memo.depths.insert(k.clone(), d);
            (k.as_slice(), *v, d)
        })
        .collect();
    let mut fresh = Vec::new();
    let root = match keyed.iter().map(|e| e.2).max() {
        None => {
            let b = node(None, &[]);
            let c = cid_of(&b);
            memo.nodes.insert(c, (b.clone(), Vec::new()));
            fresh.push((c, b));
            c
        }
        Some(level) => memo_node(&keyed, level, &mut old, memo, &mut fresh).1,
    };
    if only_new {
        fresh.reverse();
        return Ok((root, fresh));
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(c) = stack.pop() {
        let (b, kids) = &memo.nodes[&c];
        out.push((c, b.clone()));
        stack.extend(kids.iter().rev().map(|(_, c)| *c));
    }
    Ok((root, out))
}

fn memo_node(
    keys: &[(&[u8], Cid, u32)],
    level: u32,
    old: &mut Memo,
    memo: &mut Memo,
    fresh: &mut Vec<(Cid, Vec<u8>)>,
) -> (u128, Cid) {
    let f = fingerprint(keys, level);
    if let Some(c) = memo.runs.get(&f) {
        return (f, *c);
    }
    if let Some(c) = old.runs.remove(&f) {
        // Reused: move this node and everything under it (nodes and their runs) into the new memo.
        let mut stack = vec![(f, c)];
        while let Some((xf, x)) = stack.pop() {
            memo.runs.insert(xf, x);
            if memo.nodes.contains_key(&x) {
                continue;
            }
            if let Some(n) = old.nodes.remove(&x) {
                stack.extend(n.1.iter().copied());
                memo.nodes.insert(x, n);
            }
        }
        return (f, c);
    }
    let mut kids = Vec::new();
    let mut sub = |run: &[(&[u8], Cid, u32)],
                   old: &mut Memo,
                   memo: &mut Memo,
                   fresh: &mut Vec<_>|
     -> Option<Cid> {
        if run.is_empty() {
            None
        } else {
            let (rf, c) = memo_node(run, level - 1, old, memo, fresh);
            kids.push((rf, c));
            Some(c)
        }
    };
    let mut entries = Vec::new();
    let mut start = 0;
    let mut l = None;
    let mut prev: &[u8] = &[];
    let at: Vec<usize> = (0..keys.len()).filter(|i| keys[*i].2 == level).collect();
    for (n, &i) in at.iter().enumerate() {
        let left = sub(&keys[start..i], old, memo, fresh);
        if n == 0 {
            l = left;
        } else if let Some(Ipld::Map(m)) = entries.last_mut() {
            m.insert("t".to_string(), link(left));
        }
        let (k, v, _) = keys[i];
        let p = if n == 0 { 0 } else { common(prev, k) };
        let mut m = BTreeMap::new();
        m.insert("p".to_string(), Ipld::Integer(p as i128));
        m.insert("k".to_string(), Ipld::Bytes(k[p..].to_vec()));
        m.insert("v".to_string(), Ipld::Link(v));
        m.insert("t".to_string(), Ipld::Null);
        entries.push(Ipld::Map(m));
        prev = k;
        start = i + 1;
    }
    let tail = sub(&keys[start..], old, memo, fresh);
    if at.is_empty() {
        l = tail;
    } else if let Some(Ipld::Map(m)) = entries.last_mut() {
        m.insert("t".to_string(), link(tail));
    }
    let b = node(l, &entries);
    let c = cid_of(&b);
    if let std::collections::hash_map::Entry::Vacant(v) = memo.nodes.entry(c) {
        fresh.push((c, b.clone()));
        v.insert((b, kids));
    }
    memo.runs.insert(f, c);
    (f, c)
}

/// Every (key, value) under `root`, in key order, each node fetched, checked against its CID and against the layout
/// rules: keys strictly increasing, every key at its node's depth, subtrees one level down, no empty node except an
/// empty tree's root or a link-only node between two levels.
pub fn read(s: &dyn BlockStore, root: &Cid) -> R<Vec<(Vec<u8>, Cid)>> {
    let mut out = Vec::new();
    let top = load(s, root)?;
    match top.entries.first() {
        None => {
            if top.l.is_some() {
                return Err(format!(
                    "MST root {root} has no entries but a subtree: the top is not pruned"
                ));
            }
            Ok(out)
        }
        Some((k, ..)) => {
            let level = depth(k);
            walk(s, top, level, &mut out)?;
            Ok(out)
        }
    }
}

struct Node {
    l: Option<Cid>,
    /// (full key, value, right subtree)
    entries: Vec<(Vec<u8>, Cid, Option<Cid>)>,
}

fn opt_link(v: Option<&Ipld>, what: &str) -> R<Option<Cid>> {
    match v {
        Some(Ipld::Null) => Ok(None),
        Some(Ipld::Link(c)) if c.codec() == DAG_CBOR => Ok(Some(*c)),
        _ => Err(format!("MST node: {what} is not a dag-cbor link or null")),
    }
}

fn load(s: &dyn BlockStore, c: &Cid) -> R<Node> {
    if c.codec() != DAG_CBOR {
        return Err(format!("MST node {c} is not dag-cbor"));
    }
    let b = fetch(s, c)?;
    let v: Ipld = serde_ipld_dagcbor::from_slice(&b).map_err(|e| format!("MST node {c}: {e}"))?;
    let m = crate::check::exact(&v, &["l", "e"], "MST node")?;
    let l = opt_link(m.get("l"), "l")?;
    let Ipld::List(es) = &m["e"] else {
        return Err(format!("MST node {c}: e is not a list"));
    };
    let mut entries = Vec::with_capacity(es.len());
    let mut prev: Vec<u8> = Vec::new();
    for (i, e) in es.iter().enumerate() {
        let em = crate::check::exact(e, &["p", "k", "v", "t"], "MST entry")?;
        let (Ipld::Integer(p), Ipld::Bytes(k)) = (&em["p"], &em["k"]) else {
            return Err(format!("MST node {c}: entry {i} has a bad p or k"));
        };
        let p = usize::try_from(*p).map_err(|_| format!("MST node {c}: negative p"))?;
        if (i == 0 && p != 0) || p > prev.len() {
            return Err(format!(
                "MST node {c}: entry {i}'s prefix length {p} is not within the previous key"
            ));
        }
        let mut key = prev[..p].to_vec();
        key.extend_from_slice(k);
        let Some(Ipld::Link(v)) = em.get("v") else {
            return Err(format!("MST node {c}: entry {i} has no value link"));
        };
        let t = opt_link(em.get("t"), "t")?;
        prev.clone_from(&key);
        entries.push((key, *v, t));
    }
    Ok(Node { l, entries })
}

fn walk(s: &dyn BlockStore, n: Node, level: u32, out: &mut Vec<(Vec<u8>, Cid)>) -> R<()> {
    if n.entries.is_empty() && n.l.is_none() {
        return Err("MST: an empty node below the root".into());
    }
    let child = |c: Option<Cid>, out: &mut Vec<(Vec<u8>, Cid)>| -> R<()> {
        let Some(c) = c else { return Ok(()) };
        if level == 0 {
            return Err("MST: a node at depth 0 has a subtree".into());
        }
        let sub = load(s, &c)?;
        walk(s, sub, level - 1, out)
    };
    child(n.l, out)?;
    for (k, v, t) in n.entries {
        if depth(&k) != level {
            return Err(format!(
                "MST: key {} is at depth {} in a node of depth {level}",
                hex(&k),
                depth(&k)
            ));
        }
        if let Some((last, _)) = out.last()
            && *last >= k
        {
            return Err(format!("MST: key {} is out of order", hex(&k)));
        }
        out.push((k, v));
        child(t, out)?;
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::{MemoryStore, cid_with};

    fn tree(n: usize) -> Vec<(Vec<u8>, Cid)> {
        let mut v: Vec<_> = (0..n)
            .map(|i| {
                let k = format!("key{i:06}").into_bytes();
                let c = cid_with(DAG_CBOR, &k);
                (k, c)
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn round_trip_and_history_independent() {
        for n in [0, 1, 2, 5, 100, 5000] {
            let t = tree(n);
            let (root, blocks) = build(&t).unwrap();
            let mut s = MemoryStore::default();
            for (c, b) in &blocks {
                s.put(*c, b.clone());
            }
            assert_eq!(read(&s, &root).unwrap(), t, "n = {n}");
            assert_eq!(blocks[0].0, root);
        }
        // Same keys, same root, however they were gathered.
        let mut t = tree(300);
        let a = build(&t).unwrap().0;
        t.remove(17);
        let b = build(&t).unwrap().0;
        assert_ne!(a, b);
        t.insert(17, tree(300)[17].clone());
        assert_eq!(build(&t).unwrap().0, a);
    }

    #[test]
    fn memo_matches_plain_build() {
        let sorted = |b: &[(Cid, Vec<u8>)]| {
            let mut v: Vec<_> = b.iter().map(|x| x.0).collect();
            v.sort();
            v
        };
        let mut memo = Memo::default();
        let mut t = tree(3000);
        for step in 0..6 {
            match step {
                1 => t[100].1 = cid_with(DAG_CBOR, b"changed"),
                2 => {
                    t.remove(2000);
                }
                3 => t.insert(5, (b"key000004a".to_vec(), cid_with(DAG_CBOR, b"new"))),
                4 => t.truncate(10),
                _ => {}
            }
            let (root, all) = build(&t).unwrap();
            let (mroot, mall) = build_memo(&t, &mut memo, false).unwrap();
            assert_eq!((mroot, sorted(&mall)), (root, sorted(&all)), "step {step}");
            assert_eq!(mall[0].0, root);
            // The new blocks are exactly those a store holding every earlier tree lacks.
            let (nroot, new) = build_memo(&t, &mut Memo::default(), true).unwrap();
            assert_eq!(nroot, root);
            assert_eq!(sorted(&new), sorted(&all));
            let mut m2 = std::mem::take(&mut memo);
            let (_, fresh) = build_memo(&t, &mut m2, true).unwrap();
            memo = m2;
            assert!(
                fresh.is_empty(),
                "step {step}: a second build over the same entries makes nothing new"
            );
        }
    }

    #[test]
    fn refuses_unsorted_and_tampered() {
        let mut t = tree(10);
        t.swap(2, 3);
        assert!(build(&t).is_err());
        // A node whose single key is at the wrong depth: put a depth-0 key in a node claimed as a root with a subtree.
        let t = tree(50);
        let (root, blocks) = build(&t).unwrap();
        let mut s = MemoryStore::default();
        for (c, b) in &blocks {
            s.put(*c, b.clone());
        }
        // Rewrite the root's first key so it sorts wrong.
        let v: Ipld = serde_ipld_dagcbor::from_slice(&blocks[0].1).unwrap();
        let Ipld::Map(mut m) = v else { panic!() };
        let Some(Ipld::List(es)) = m.get_mut("e") else {
            panic!()
        };
        let Ipld::Map(e0) = &mut es[0] else { panic!() };
        e0.insert("k".into(), Ipld::Bytes(b"zzzz".to_vec()));
        let bad = encode(&Ipld::Map(m));
        let bc = cid_of(&bad);
        s.put(bc, bad);
        assert!(read(&s, &bc).is_err());
        assert!(read(&s, &root).is_ok());
    }
}
