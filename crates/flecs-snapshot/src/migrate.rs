//! Migrations as data (SPEC.md, "Migrations"): edges between layout records keyed by (old hash, new hash), found in
//! chains, applied to values in their snapshot shape. The default conversion goes by member and constant name.

use std::collections::{BTreeMap, HashMap, VecDeque};

use ipld_core::ipld::Ipld;

use crate::check::{self, Blobs};
use crate::dag::VERSION;
use crate::walk::{encode, member_err};

pub type R<T> = Result<T, String>;
pub type Hash = [u8; 8];

/// What a restore or a live migration did beyond copying values.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Members an old layout had and the new one does not, as `Type.member` (their values were dropped).
    pub dropped: Vec<String>,
    /// Types whose values were migrated, as `Old -> New`.
    pub migrated: Vec<String>,
}

/// A custom conversion: the old value to the new one, both in their snapshot shape.
pub type Custom = Box<dyn Fn(&Ipld) -> R<Ipld>>;

enum Convert {
    /// New member path to old member name.
    ByName(BTreeMap<String, String>),
    Custom(Custom),
}

struct Edge {
    from: Ipld,
    to: Ipld,
    convert: Convert,
}

/// A set of migration edges. Edges are keyed by (old layout hash, new layout hash); one per key.
#[derive(Default)]
pub struct Migrations {
    edges: BTreeMap<(Hash, Hash), Edge>,
    /// Old hash to the new hashes it has edges to.
    out: HashMap<Hash, Vec<Hash>>,
}

/// A layout record's hash.
pub fn hash_of(rec: &Ipld) -> R<Hash> {
    match rec {
        Ipld::Map(m) => match m.get("hash") {
            Some(Ipld::Bytes(b)) => b
                .as_slice()
                .try_into()
                .map_err(|_| "a layout hash is not 8 bytes".into()),
            _ => Err("a layout record has no hash".into()),
        },
        _ => Err("a layout record is not a map".into()),
    }
}

/// The type path a layout record belongs to: its base, plus `V<version>` past version 1.
pub fn path_of(rec: &Ipld) -> R<String> {
    let Ipld::Map(m) = rec else {
        return Err("a layout record is not a map".into());
    };
    match (m.get("base"), m.get("version")) {
        (Some(Ipld::String(b)), Some(Ipld::Integer(1))) => Ok(b.clone()),
        (Some(Ipld::String(b)), Some(Ipld::Integer(v))) => Ok(format!("{b}V{v}")),
        _ => Err("a layout record has no base or version".into()),
    }
}

fn layout(rec: &Ipld) -> &Ipld {
    match rec {
        Ipld::Map(m) => &m["layout"],
        _ => unreachable!("checked records are maps"),
    }
}

fn checked(rec: &Ipld) -> R<Hash> {
    let p = path_of(rec)?;
    check::layout_record(&p, rec)?;
    hash_of(rec)
}

impl Migrations {
    pub fn new() -> Migrations {
        Migrations::default()
    }

    fn add(&mut self, from: Ipld, to: Ipld, convert: Convert) -> R<()> {
        let (a, b) = (checked(&from)?, checked(&to)?);
        if a == b {
            return Err(format!("a migration from {} to itself", path_of(&from)?));
        }
        if self.edges.contains_key(&(a, b)) {
            return Err(format!(
                "two migrations from {} to {} with the same layouts",
                path_of(&from)?,
                path_of(&to)?
            ));
        }
        self.edges.insert((a, b), Edge { from, to, convert });
        self.out.entry(a).or_default().push(b);
        Ok(())
    }

    /// An edge converted by name (SPEC.md, "Migrations"). `renamed_from` maps a new member path (`pos.x`) to the old
    /// member's name in the same struct.
    pub fn by_name(&mut self, from: Ipld, to: Ipld, renamed_from: &[(&str, &str)]) -> R<()> {
        let renamed = renamed_from
            .iter()
            .map(|(n, o)| (n.to_string(), o.to_string()))
            .collect();
        self.add(from, to, Convert::ByName(renamed))
    }

    /// An edge converted by `f`, whose result must fit the new layout.
    pub fn custom(&mut self, from: Ipld, to: Ipld, f: Custom) -> R<()> {
        self.add(from, to, Convert::Custom(f))
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// The edges' records reachable from `from`, nearest first, with the chain of hashes leading to each.
    pub fn reachable(&self, from: &Hash) -> Vec<(Vec<Hash>, &Ipld)> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::from([*from]);
        let mut q = VecDeque::from([vec![*from]]);
        while let Some(chain) = q.pop_front() {
            let last = *chain.last().unwrap();
            for next in self.out.get(&last).into_iter().flatten() {
                if !seen.insert(*next) {
                    continue;
                }
                let mut c = chain.clone();
                c.push(*next);
                out.push((c.clone(), &self.edges[&(last, *next)].to));
                q.push_back(c);
            }
        }
        out
    }

    /// The record an edge leaves from, by hash (for reading a value saved with a layout no world has).
    pub fn record(&self, h: &Hash) -> Option<&Ipld> {
        self.edges.iter().find_map(|((a, b), e)| {
            if a == h {
                Some(&e.from)
            } else if b == h {
                Some(&e.to)
            } else {
                None
            }
        })
    }

    /// The shortest chain of hashes from `from` to `to`, both ends included.
    pub fn chain(&self, from: &Hash, to: &Hash) -> Option<Vec<Hash>> {
        if from == to {
            return Some(vec![*from]);
        }
        self.reachable(from)
            .into_iter()
            .find(|(c, _)| c.last() == Some(to))
            .map(|(c, _)| c)
    }

    /// Converts `v` along `chain`. `default` gives, for an edge's new record, the value a live world's constructor makes
    /// for that type, when one has it; members only the new layout has take their part of it, or zero without one.
    pub fn convert_chain(
        &self,
        chain: &[Hash],
        v: &Ipld,
        default: &mut dyn FnMut(&Ipld) -> Option<Ipld>,
        report: &mut Report,
    ) -> R<Ipld> {
        let mut cur = v.clone();
        for w in chain.windows(2) {
            let e = &self.edges[&(w[0], w[1])];
            let (fp, tp) = (path_of(&e.from)?, path_of(&e.to)?);
            let what = format!("migrating {fp} to {tp}");
            cur = match &e.convert {
                Convert::ByName(renamed) => {
                    let d = default(&e.to);
                    let mut cx = ByName {
                        renamed,
                        dropped: Vec::new(),
                    };
                    let r = cx
                        .value(layout(&e.from), layout(&e.to), &cur, "", d.as_ref())
                        .map_err(|m| format!("{what}: {m}"))?;
                    report
                        .dropped
                        .extend(cx.dropped.into_iter().map(|m| format!("{fp}.{m}")));
                    r
                }
                Convert::Custom(f) => f(&cur).map_err(|m| format!("{what}: {m}"))?,
            };
            check::value(&cur, layout(&e.to), Blobs::Links)
                .map_err(|m| format!("{what}: the result does not fit {tp}: {m}"))?;
            let line = format!("{fp} -> {tp}");
            if !report.migrated.contains(&line) {
                report.migrated.push(line);
            }
        }
        Ok(cur)
    }

    /// Converts `v` from layout `from` to layout `to` along the shortest chain, zero defaults.
    pub fn convert(&self, from: &Hash, to: &Hash, v: &Ipld, report: &mut Report) -> R<Ipld> {
        let chain = self
            .chain(from, to)
            .ok_or("no migration chain between these layouts")?;
        self.convert_chain(&chain, v, &mut |_| None, report)
    }

    /// The by-name edges as DAG-CBOR: `{"version", "edges": [{"from", "to", "renamed_from"}]}`. Custom edges are
    /// code and are left out.
    pub fn to_cbor(&self) -> Vec<u8> {
        let edges = self
            .edges
            .values()
            .filter_map(|e| match &e.convert {
                Convert::ByName(r) => {
                    let mut m = BTreeMap::new();
                    m.insert("from".to_string(), e.from.clone());
                    m.insert("to".to_string(), e.to.clone());
                    m.insert(
                        "renamed_from".to_string(),
                        Ipld::Map(
                            r.iter()
                                .map(|(k, v)| (k.clone(), Ipld::String(v.clone())))
                                .collect(),
                        ),
                    );
                    Some(Ipld::Map(m))
                }
                Convert::Custom(_) => None,
            })
            .collect();
        let mut m = BTreeMap::new();
        m.insert("version".to_string(), Ipld::Integer(VERSION as i128));
        m.insert("edges".to_string(), Ipld::List(edges));
        encode(&Ipld::Map(m))
    }

    pub fn from_cbor(b: &[u8]) -> R<Migrations> {
        let v: Ipld = serde_ipld_dagcbor::from_slice(b).map_err(|e| format!("migrations: {e}"))?;
        let m = check::exact(&v, &["version", "edges"], "migrations")?;
        check::version(m, "migrations")?;
        let Ipld::List(es) = &m["edges"] else {
            return Err("migrations: edges is not a list".into());
        };
        let mut out = Migrations::new();
        for e in es {
            let em = check::exact(e, &["from", "to", "renamed_from"], "migration edge")?;
            let Ipld::Map(r) = &em["renamed_from"] else {
                return Err("migration edge: renamed_from is not a map".into());
            };
            let mut renamed = BTreeMap::new();
            for (k, v) in r {
                let Ipld::String(v) = v else {
                    return Err("migration edge: a renamed_from value is not text".into());
                };
                renamed.insert(k.clone(), v.clone());
            }
            out.add(
                em["from"].clone(),
                em["to"].clone(),
                Convert::ByName(renamed),
            )?;
        }
        Ok(out)
    }
}

struct ByName<'a> {
    renamed: &'a BTreeMap<String, String>,
    dropped: Vec<String>,
}

fn int_kind(p: &str) -> Option<(bool, u32)> {
    // (signed, bits)
    Some(match p {
        "u8" | "byte" => (false, 8),
        "u16" => (false, 16),
        "u32" => (false, 32),
        "u64" | "uptr" => (false, 64),
        "i8" | "char" => (true, 8),
        "i16" => (true, 16),
        "i32" => (true, 32),
        "i64" | "iptr" => (true, 64),
        _ => return None,
    })
}

/// Whether a value of primitive `a` converts to `b` without loss for every value (SPEC.md, "Migrations").
fn widens(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (int_kind(a), int_kind(b)) {
        (Some((false, x)), Some((false, y))) => return y >= x,
        (Some((false, x)), Some((true, y))) => return y > x,
        (Some((true, x)), Some((true, y))) => return y >= x,
        (Some(_), Some(_)) => return false,
        (Some((_, x)), None) => return b == "f64" && x <= 32,
        _ => {}
    }
    matches!((a, b), ("f32", "f64") | ("f16" | "bf16", "f32" | "f64"))
}

fn members(l: &BTreeMap<String, Ipld>) -> Vec<(&str, &Ipld)> {
    match &l["members"] {
        Ipld::List(ms) => ms
            .iter()
            .filter_map(|m| match m {
                Ipld::List(m) => match (&m[0], &m[1]) {
                    (Ipld::String(n), l) => Some((n.as_str(), l)),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn names(l: &BTreeMap<String, Ipld>) -> Vec<(&str, i128)> {
    match &l["constants"] {
        Ipld::List(cs) => cs
            .iter()
            .filter_map(|c| match c {
                Ipld::List(c) => match c.as_slice() {
                    [Ipld::String(n), Ipld::Integer(v)] => Some((n.as_str(), *v)),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn kind(l: &Ipld) -> &'static str {
    match l {
        Ipld::String(_) => "primitive",
        Ipld::Map(m) if m.contains_key("struct") => "struct",
        Ipld::Map(m) if m.contains_key("enum") => "enum",
        Ipld::Map(m) if m.contains_key("bitmask") => "bitmask",
        Ipld::Map(m) if m.contains_key("array") => "array",
        Ipld::Map(m) if m.contains_key("vector") => "vector",
        Ipld::Map(m) if m.contains_key("option") => "option",
        Ipld::Map(m) if m.contains_key("union") => "union",
        Ipld::Map(m) if m.contains_key("map") => "map",
        Ipld::Map(m) if m.contains_key("runtime") => "runtime",
        _ => "unknown",
    }
}

fn describe(l: &Ipld) -> String {
    match l {
        Ipld::String(p) => p.clone(),
        l => kind(l).to_string(),
    }
}

/// The zero value of a layout: false, 0, empty, null, the constant whose value is 0 (else the first).
pub fn zero(l: &Ipld) -> R<Ipld> {
    Ok(match l {
        Ipld::String(p) => match p.as_str() {
            "bool" => Ipld::Bool(false),
            "f32" | "f64" | "f16" | "bf16" => Ipld::Float(0.0),
            "string" => Ipld::String(String::new()),
            "entity" | "id" => Ipld::Null,
            "bytes" => Ipld::Bytes(Vec::new()),
            "blob" => return Err("a new Blob member has no default".into()),
            p if int_kind(p).is_some() => Ipld::Integer(0),
            p => return Err(format!("unknown primitive {p}")),
        },
        Ipld::Map(m) if m.contains_key("struct") => Ipld::Map(
            members(m)
                .into_iter()
                .map(|(n, ml)| Ok((n.to_string(), zero(ml)?)))
                .collect::<R<_>>()?,
        ),
        Ipld::Map(m) if m.contains_key("enum") => {
            let cs = names(m);
            let c = cs
                .iter()
                .find(|(_, v)| *v == 0)
                .or(cs.first())
                .ok_or("an enum with no constants")?;
            Ipld::String(c.0.to_string())
        }
        Ipld::Map(m) if m.contains_key("bitmask") => Ipld::List(Vec::new()),
        Ipld::Map(m) if m.contains_key("array") => {
            let Ipld::Integer(n) = m["count"] else {
                return Err("bad array count".into());
            };
            let z = zero(&m["array"])?;
            Ipld::List(vec![z; n as usize])
        }
        Ipld::Map(m)
            if m.contains_key("vector") || m.contains_key("option") || m.contains_key("map") =>
        {
            Ipld::List(Vec::new())
        }
        Ipld::Map(m) if m.contains_key("runtime") => Ipld::Null,
        Ipld::Map(m) if m.contains_key("union") => {
            // The first variant, with its payload's zero.
            let Some(Ipld::List(vs)) = m.get("variants") else {
                return Err("bad union".into());
            };
            let Some(Ipld::List(v)) = vs.first() else {
                return Err("a union with no variants".into());
            };
            let (Some(Ipld::String(n)), Some(pl)) = (v.first(), v.get(1)) else {
                return Err("bad variant".into());
            };
            let z = match pl {
                Ipld::Null => Ipld::Null,
                pl => zero(pl)?,
            };
            Ipld::Map(BTreeMap::from([(n.clone(), z)]))
        }
        _ => return Err("unknown layout".into()),
    })
}

impl ByName<'_> {
    /// `v` (of layout `a`) as a value of layout `b`. `at` is the member path so far; `default` is the new type's
    /// default value at this place, when known.
    fn value(&mut self, a: &Ipld, b: &Ipld, v: &Ipld, at: &str, default: Option<&Ipld>) -> R<Ipld> {
        if a == b {
            return Ok(v.clone());
        }
        match (a, b) {
            (Ipld::String(x), Ipld::String(y)) => {
                if !widens(x, y) {
                    return Err(format!("{x} cannot become {y} without loss"));
                }
                Ok(match (v, int_kind(y)) {
                    (Ipld::Integer(i), None) => Ipld::Float(*i as f64),
                    _ => v.clone(),
                })
            }
            (Ipld::Map(x), Ipld::Map(y)) if kind(a) == "struct" && kind(b) == "struct" => {
                let Ipld::Map(vm) = v else {
                    return Err("a struct value is not a map".into());
                };
                let old: BTreeMap<&str, &Ipld> = members(x).into_iter().collect();
                let mut used = std::collections::BTreeSet::new();
                let mut out = BTreeMap::new();
                for (n, nl) in members(y) {
                    let path = if at.is_empty() {
                        n.to_string()
                    } else {
                        format!("{at}.{n}")
                    };
                    let src = self.renamed.get(&path).map_or(n, String::as_str);
                    let d = match default {
                        Some(Ipld::Map(dm)) => dm.get(n),
                        _ => None,
                    };
                    let nv = match (old.get(src), vm.get(src)) {
                        (Some(ol), Some(ov)) => {
                            used.insert(src);
                            self.value(ol, nl, ov, &path, d)
                                .map_err(|e| member_err(n, e))?
                        }
                        _ => match d {
                            Some(d) => d.clone(),
                            None => zero(nl).map_err(|e| member_err(n, e))?,
                        },
                    };
                    out.insert(n.to_string(), nv);
                }
                for (n, _) in members(x) {
                    if !used.contains(n) {
                        self.dropped.push(if at.is_empty() {
                            n.to_string()
                        } else {
                            format!("{at}.{n}")
                        });
                    }
                }
                Ok(Ipld::Map(out))
            }
            (Ipld::Map(_), Ipld::Map(y)) if kind(a) == "enum" && kind(b) == "enum" => {
                let Ipld::String(n) = v else {
                    return Err("an enum value is not a name".into());
                };
                if names(y).iter().any(|(c, _)| c == n) {
                    Ok(v.clone())
                } else {
                    Err(format!("the new enum has no constant {n:?}"))
                }
            }
            (Ipld::Map(_), Ipld::Map(y)) if kind(a) == "bitmask" && kind(b) == "bitmask" => {
                let Ipld::List(ns) = v else {
                    return Err("a bitmask value is not a list".into());
                };
                let known = names(y);
                for n in ns {
                    if !known.iter().any(|(c, _)| Ipld::String(c.to_string()) == *n) {
                        return Err(format!("the new bitmask has no constant {n:?}"));
                    }
                }
                Ok(v.clone())
            }
            (Ipld::Map(x), Ipld::Map(y))
                if matches!(kind(a), "array" | "vector")
                    && matches!(kind(b), "array" | "vector") =>
            {
                let Ipld::List(items) = v else {
                    return Err("a collection value is not a list".into());
                };
                let (ae, be) = (
                    x.get("array").or(x.get("vector")).unwrap(),
                    y.get("array").or(y.get("vector")).unwrap(),
                );
                if let Some(Ipld::Integer(n)) = y.get("count") {
                    match x.get("count") {
                        Some(Ipld::Integer(m)) if m == n => {}
                        _ => {
                            return Err(format!(
                                "a {} cannot become an array of {n} (only an array of the same count can)",
                                kind(a)
                            ));
                        }
                    }
                }
                items
                    .iter()
                    .enumerate()
                    .map(|(i, x)| {
                        self.value(ae, be, x, at, None)
                            .map_err(|e| member_err(&format!("[{i}]"), e))
                    })
                    .collect::<R<_>>()
                    .map(Ipld::List)
            }
            (Ipld::Map(x), Ipld::Map(y)) if kind(a) == "option" && kind(b) == "option" => match v {
                Ipld::List(items) if items.len() <= 1 => items
                    .iter()
                    .map(|i| self.value(&x["option"], &y["option"], i, at, None))
                    .collect::<R<_>>()
                    .map(Ipld::List),
                _ => Err("an option value is not a list of at most one".into()),
            },
            (_, Ipld::Map(y)) if kind(b) == "option" => {
                // A value becomes an option of its type: some.
                Ok(Ipld::List(vec![self.value(
                    a,
                    &y["option"],
                    v,
                    at,
                    None,
                )?]))
            }
            (Ipld::Map(x), _) if kind(a) == "option" => match v {
                Ipld::List(items) if items.len() == 1 => {
                    self.value(&x["option"], b, &items[0], at, None)
                }
                Ipld::List(items) if items.is_empty() => Err(format!(
                    "an option with no value cannot become a {}",
                    describe(b)
                )),
                _ => Err("an option value is not a list of at most one".into()),
            },
            (Ipld::Map(x), Ipld::Map(y)) if kind(a) == "union" && kind(b) == "union" => {
                let Ipld::Map(vm) = v else {
                    return Err("a union value is not a map".into());
                };
                let Some((n, pv)) = vm.iter().next() else {
                    return Err("a union value has no variant".into());
                };
                let payload = |l: &BTreeMap<String, Ipld>| -> Option<Ipld> {
                    let Ipld::List(vs) = &l["variants"] else {
                        return None;
                    };
                    vs.iter().find_map(|var| match var {
                        Ipld::List(var) if var.first() == Some(&Ipld::String(n.clone())) => {
                            var.get(1).cloned()
                        }
                        _ => None,
                    })
                };
                let (Some(pa), Some(pb)) = (payload(x), payload(y)) else {
                    return Err(format!("the new union has no variant {n:?}"));
                };
                let path = if at.is_empty() {
                    n.clone()
                } else {
                    format!("{at}.{n}")
                };
                let nv = match (&pa, &pb) {
                    (Ipld::Null, Ipld::Null) => Ipld::Null,
                    (Ipld::Null, pb) => zero(pb).map_err(|e| member_err(n, e))?,
                    (_, Ipld::Null) => {
                        self.dropped.push(path);
                        Ipld::Null
                    }
                    (pa, pb) => self
                        .value(pa, pb, pv, &path, None)
                        .map_err(|e| member_err(n, e))?,
                };
                Ok(Ipld::Map(BTreeMap::from([(n.clone(), nv)])))
            }
            (Ipld::Map(x), Ipld::Map(y)) if kind(a) == "map" && kind(b) == "map" => {
                let (Some(Ipld::List(ka)), Some(Ipld::List(kb))) = (x.get("map"), y.get("map"))
                else {
                    return Err("bad map layout".into());
                };
                let Ipld::List(es) = v else {
                    return Err("a map value is not a list".into());
                };
                let mut out = Vec::with_capacity(es.len());
                for (i, e) in es.iter().enumerate() {
                    let Ipld::List(kv) = e else {
                        return Err("a map entry is not [key, value]".into());
                    };
                    let k = self
                        .value(&ka[0], &kb[0], &kv[0], at, None)
                        .map_err(|e| member_err(&format!("[{i}].key"), e))?;
                    let val = self
                        .value(&ka[1], &kb[1], &kv[1], at, None)
                        .map_err(|e| member_err(&format!("[{i}].value"), e))?;
                    out.push((encode(&k), k, val));
                }
                out.sort_by(|p, q| p.0.cmp(&q.0));
                if out.windows(2).any(|w| w[0].0 == w[1].0) {
                    return Err("two keys of the map convert to one".into());
                }
                Ok(Ipld::List(
                    out.into_iter()
                        .map(|(_, k, v)| Ipld::List(vec![k, v]))
                        .collect(),
                ))
            }
            (_, Ipld::Map(_)) if kind(b) == "runtime" => Ok(Ipld::Null),
            _ => Err(format!("a {} cannot become a {}", describe(a), describe(b))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walk::hash8;

    fn rec(path: &str, l: Ipld) -> Ipld {
        let (base, version) = check::base_version(path).unwrap();
        let mut m = BTreeMap::new();
        m.insert("base".into(), Ipld::String(base));
        m.insert("version".into(), Ipld::Integer(version as i128));
        m.insert("layout".into(), l);
        m.insert("merge".into(), Ipld::Null);
        m.insert("sync".into(), Ipld::String("Synced".into()));
        m.insert("writable_by".into(), Ipld::Null);
        m.insert("validated_by".into(), Ipld::Null);
        let h = hash8(&encode(&Ipld::Map(m.clone())));
        m.insert("hash".into(), Ipld::Bytes(h));
        Ipld::Map(m)
    }

    fn st(name: &str, ms: &[(&str, Ipld)]) -> Ipld {
        let mut m = BTreeMap::new();
        m.insert("struct".into(), Ipld::String(name.into()));
        m.insert(
            "members".into(),
            Ipld::List(
                ms.iter()
                    .map(|(n, l)| {
                        Ipld::List(vec![
                            Ipld::String(n.to_string()),
                            l.clone(),
                            Ipld::String("LastWriter".into()),
                        ])
                    })
                    .collect(),
            ),
        );
        Ipld::Map(m)
    }

    fn s(x: &str) -> Ipld {
        Ipld::String(x.into())
    }

    fn val(kv: &[(&str, Ipld)]) -> Ipld {
        Ipld::Map(kv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    #[test]
    fn chain_by_name() {
        let v1 = rec("Hp", st("Hp", &[("hp", s("u16")), ("old", s("bool"))]));
        let v2 = rec(
            "HpV2",
            st("HpV2", &[("health", s("u32")), ("max", s("u32"))]),
        );
        let v3 = rec(
            "HpV3",
            st(
                "HpV3",
                &[
                    ("health", s("f64")),
                    ("max", s("u64")),
                    ("tag", s("string")),
                ],
            ),
        );
        let mut m = Migrations::new();
        m.by_name(v1.clone(), v2.clone(), &[("health", "hp")])
            .unwrap();
        m.by_name(v2.clone(), v3.clone(), &[]).unwrap();
        let mut r = Report::default();
        let out = m
            .convert(
                &hash_of(&v1).unwrap(),
                &hash_of(&v3).unwrap(),
                &val(&[("hp", Ipld::Integer(7)), ("old", Ipld::Bool(true))]),
                &mut r,
            )
            .unwrap();
        assert_eq!(
            out,
            val(&[
                ("health", Ipld::Float(7.0)),
                ("max", Ipld::Integer(0)),
                ("tag", s(""))
            ])
        );
        assert_eq!(r.dropped, vec!["Hp.old".to_string()]);
        assert_eq!(r.migrated, vec!["Hp -> HpV2", "HpV2 -> HpV3"]);
        // Round trip through CBOR.
        let back = Migrations::from_cbor(&m.to_cbor()).unwrap();
        assert!(
            back.chain(&hash_of(&v1).unwrap(), &hash_of(&v3).unwrap())
                .is_some()
        );
    }

    #[test]
    fn impossible_is_an_error() {
        let a = rec("N", st("N", &[("x", s("u64"))]));
        let b = rec("NV2", st("NV2", &[("x", s("u32"))]));
        let mut m = Migrations::new();
        m.by_name(a.clone(), b.clone(), &[]).unwrap();
        let e = m
            .convert(
                &hash_of(&a).unwrap(),
                &hash_of(&b).unwrap(),
                &val(&[("x", Ipld::Integer(1))]),
                &mut Report::default(),
            )
            .unwrap_err();
        assert!(e.contains("u64 cannot become u32"), "{e}");
        let c = rec("NV3", st("NV3", &[("x", s("string"))]));
        m.by_name(b.clone(), c.clone(), &[]).unwrap();
        assert!(
            m.convert(
                &hash_of(&b).unwrap(),
                &hash_of(&c).unwrap(),
                &val(&[("x", Ipld::Integer(1))]),
                &mut Report::default()
            )
            .is_err()
        );
    }

    #[test]
    fn described_kinds() {
        let opt = |l: Ipld| Ipld::Map(BTreeMap::from([("option".to_string(), l)]));
        let uni = |vs: Vec<(&str, Ipld)>| {
            Ipld::Map(BTreeMap::from([
                ("union".to_string(), s("Shape")),
                (
                    "variants".to_string(),
                    Ipld::List(
                        vs.into_iter()
                            .map(|(n, l)| Ipld::List(vec![s(n), l]))
                            .collect(),
                    ),
                ),
            ]))
        };
        let inner = |t: &str| {
            Ipld::Map(BTreeMap::from([
                ("struct".to_string(), Ipld::Null),
                (
                    "members".to_string(),
                    Ipld::List(vec![Ipld::List(vec![s("r"), s(t), Ipld::Null])]),
                ),
            ]))
        };
        let a = rec(
            "S",
            st(
                "S",
                &[
                    ("o", opt(s("u8"))),
                    ("w", s("f16")),
                    ("u", uni(vec![("Dot", Ipld::Null), ("Ball", inner("f32"))])),
                ],
            ),
        );
        let b = rec(
            "SV2",
            st(
                "SV2",
                &[
                    ("o", opt(s("u32"))),
                    ("w", s("f32")),
                    ("u", uni(vec![("Ball", inner("f64")), ("Dot", Ipld::Null)])),
                ],
            ),
        );
        let mut m = Migrations::new();
        m.by_name(a.clone(), b.clone(), &[]).unwrap();
        let v = val(&[
            ("o", Ipld::List(vec![Ipld::Integer(3)])),
            ("w", Ipld::Float(0.5)),
            ("u", val(&[("Ball", val(&[("r", Ipld::Float(2.0))]))])),
        ]);
        let out = m
            .convert(
                &hash_of(&a).unwrap(),
                &hash_of(&b).unwrap(),
                &v,
                &mut Report::default(),
            )
            .unwrap();
        assert_eq!(out, v);
        let c = rec(
            "SV3",
            st(
                "SV3",
                &[
                    ("o", s("u32")),
                    ("w", s("f32")),
                    ("u", uni(vec![("Dot", Ipld::Null)])),
                ],
            ),
        );
        m.by_name(b.clone(), c.clone(), &[]).unwrap();
        // A value in a variant the new union lacks is an error.
        assert!(
            m.convert(
                &hash_of(&b).unwrap(),
                &hash_of(&c).unwrap(),
                &v,
                &mut Report::default()
            )
            .is_err()
        );
    }

    #[test]
    fn base_versions() {
        assert_eq!(check::base_version("FooV2").unwrap(), ("Foo".into(), 2));
        assert_eq!(
            check::base_version("a.b.HullV12").unwrap(),
            ("a.b.Hull".into(), 12)
        );
        assert_eq!(check::base_version("Foo").unwrap(), ("Foo".into(), 1));
        assert_eq!(check::base_version("FooV02").unwrap(), ("FooV02".into(), 1));
        assert_eq!(check::base_version("V2").unwrap(), ("V2".into(), 1));
        assert_eq!(check::base_version("a\\.V2").unwrap(), ("a\\.".into(), 2));
        assert!(check::base_version("FooV1").is_err());
    }
}
