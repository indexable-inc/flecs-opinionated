//! Compatibility of two layout records of one type (SPEC.md, "Compatibility"): what a change from the old record to
//! the new one means for the values stored with the old one. A host's version check calls [`compare`] instead of
//! comparing hashes, and [`crate::Migrations::compatible`] turns a `Safe` or `NeedsCheck` verdict into an edge.

use std::collections::{BTreeMap, BTreeSet};

use ipld_core::ipld::Ipld;

use crate::logical::{node_of, physical};

pub type R<T> = Result<T, String>;

/// What a change asks of the values stored with the old layout, from least to most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Compat {
    /// Every old value is a valid new value as it is: a rule loosened or dropped, a catalog name or an enum constant
    /// added, a unit added or dropped, a merge rule, name or permission changed.
    Safe,
    /// Every old value reads as it is, but a rule was tightened or added, so each must pass the new rules (a
    /// restore through [`crate::Migrations::compatible`]'s edge checks every one and refuses the first that fails).
    NeedsCheck,
    /// Old values do not read as new ones: a physical change (a primitive, a member or variant added or removed, an
    /// array's count, a kind), a unit changed, a catalog name removed, or the sync mode changed. Needs a migration
    /// edge (`Migrations::by_name` or `custom`).
    NeedsMigration,
}

impl Compat {
    pub fn name(self) -> &'static str {
        match self {
            Compat::Safe => "Safe",
            Compat::NeedsCheck => "NeedsCheck",
            Compat::NeedsMigration => "NeedsMigration",
        }
    }
}

/// One difference between two layout records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Where: a member path (`pos.x`, `pts[]`, `map{key}`, a union variant's name), empty for the whole value.
    pub path: String,
    pub compat: Compat,
    /// What changed, in words.
    pub what: String,
}

/// The verdict of a comparison: the worst change's class (`Safe` when nothing changed), and every change found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub compat: Compat,
    pub changes: Vec<Change>,
}

/// Classifies the change from layout record `old` to `new` (both checked as SPEC.md, "Layouts", and of one base type).
pub fn compare(old: &Ipld, new: &Ipld) -> R<Verdict> {
    let (a, b) = (record(old, "old")?, record(new, "new")?);
    if a["base"] != b["base"] {
        return Err(format!(
            "the records are of two types, {:?} and {:?}",
            a["base"], b["base"]
        ));
    }
    let mut cx = Cx::default();
    if a.get("hash") != b.get("hash") {
        if a["sync"] != b["sync"] {
            cx.add(
                "",
                Compat::NeedsMigration,
                format!("sync mode {:?} became {:?}", a["sync"], b["sync"]),
            );
        }
        if a["merge"] != b["merge"] {
            cx.add(
                "",
                Compat::Safe,
                format!("merge rule {:?} became {:?}", a["merge"], b["merge"]),
            );
        }
        for k in ["writable_by", "validated_by"] {
            if a[k] != b[k] {
                cx.add("", Compat::Safe, format!("{k} changed"));
            }
        }
        cx.layout("", &a["layout"], &b["layout"]);
    }
    let compat = cx.0.iter().map(|c| c.compat).max().unwrap_or(Compat::Safe);
    Ok(Verdict {
        compat,
        changes: cx.0,
    })
}

fn record<'a>(rec: &'a Ipld, which: &str) -> R<&'a BTreeMap<String, Ipld>> {
    let path = crate::migrate::path_of(rec).map_err(|e| format!("the {which} record: {e}"))?;
    crate::check::layout_record(&path, rec).map_err(|e| format!("the {which} record: {e}"))?;
    match rec {
        Ipld::Map(m) => Ok(m),
        _ => unreachable!("a checked record is a map"),
    }
}

#[derive(Default)]
struct Cx(Vec<Change>);

fn join(at: &str, seg: &str) -> String {
    match (at.is_empty(), seg.starts_with('[') || seg.starts_with('{')) {
        (true, _) => seg.to_string(),
        (false, true) => format!("{at}{seg}"),
        (false, false) => format!("{at}.{seg}"),
    }
}

fn kind(l: &Ipld) -> &'static str {
    match l {
        Ipld::String(_) => "primitive",
        Ipld::Map(m) => [
            "struct", "enum", "bitmask", "array", "vector", "option", "map", "union", "runtime",
        ]
        .into_iter()
        .find(|k| m.contains_key(*k))
        .unwrap_or("unknown"),
        _ => "unknown",
    }
}

fn show(l: &Ipld) -> String {
    match l {
        Ipld::String(p) => p.clone(),
        l => kind(l).to_string(),
    }
}

fn names(v: &Ipld) -> BTreeSet<String> {
    match v {
        Ipld::List(xs) => xs
            .iter()
            .filter_map(|x| match x {
                Ipld::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// A list of `[name, ...]` entries as name to the entry's other slots.
fn entries(v: Option<&Ipld>) -> BTreeMap<String, Vec<Ipld>> {
    match v {
        Some(Ipld::List(xs)) => xs
            .iter()
            .filter_map(|x| match x {
                Ipld::List(x) => match x.first() {
                    Some(Ipld::String(n)) => Some((n.clone(), x[1..].to_vec())),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// A bound's place against another: `None` (unbounded) is outside every number.
fn looser(old: &Ipld, new: &Ipld, lower: bool) -> bool {
    let n = |v: &Ipld| match v {
        Ipld::Integer(i) => Some(*i as f64),
        Ipld::Float(f) => Some(*f),
        _ => None,
    };
    match (n(old), n(new)) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(o), Some(x)) => {
            if lower {
                x <= o
            } else {
                x >= o
            }
        }
    }
}

impl Cx {
    fn add(&mut self, at: &str, compat: Compat, what: String) {
        self.0.push(Change {
            path: at.to_string(),
            compat,
            what,
        });
    }

    fn layout(&mut self, at: &str, a: &Ipld, b: &Ipld) {
        if a == b {
            return;
        }
        self.rules(at, a, b);
        let (a, b) = (physical(a), physical(b));
        if a == b {
            return;
        }
        let (ka, kb) = (kind(a), kind(b));
        let (Ipld::Map(x), Ipld::Map(y)) = (a, b) else {
            self.add(
                at,
                Compat::NeedsMigration,
                format!("{} became {}", show(a), show(b)),
            );
            return;
        };
        if ka != kb {
            self.add(at, Compat::NeedsMigration, format!("a {ka} became a {kb}"));
            return;
        }
        match ka {
            "struct" => {
                if x["struct"] != y["struct"] {
                    self.add(at, Compat::Safe, "the struct's name changed".into());
                }
                let (mx, my) = (entries(x.get("members")), entries(y.get("members")));
                let order = |m: &BTreeMap<String, Ipld>| -> Vec<String> {
                    match m.get("members") {
                        Some(Ipld::List(xs)) => xs
                            .iter()
                            .filter_map(|x| match x {
                                Ipld::List(x) => match x.first() {
                                    Some(Ipld::String(n)) => Some(n.clone()),
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect(),
                        _ => Vec::new(),
                    }
                };
                for (n, slots) in &mx {
                    let p = join(at, n);
                    match my.get(n) {
                        None => self.add(&p, Compat::NeedsMigration, "member removed".into()),
                        Some(new) => {
                            if slots.get(1) != new.get(1) {
                                self.add(
                                    &p,
                                    Compat::Safe,
                                    format!(
                                        "merge rule {:?} became {:?}",
                                        slots.get(1).unwrap_or(&Ipld::Null),
                                        new.get(1).unwrap_or(&Ipld::Null)
                                    ),
                                );
                            }
                            self.layout(&p, &slots[0], &new[0]);
                        }
                    }
                }
                for n in my.keys().filter(|n| !mx.contains_key(*n)) {
                    self.add(&join(at, n), Compat::NeedsMigration, "member added".into());
                }
                let (oa, ob): (Vec<_>, Vec<_>) = (
                    order(x)
                        .into_iter()
                        .filter(|n| my.contains_key(n))
                        .collect(),
                    order(y)
                        .into_iter()
                        .filter(|n| mx.contains_key(n))
                        .collect(),
                );
                if oa != ob {
                    self.add(at, Compat::Safe, "members reordered".into());
                }
            }
            "enum" | "bitmask" => {
                if x.get(ka) != y.get(ka) {
                    self.add(at, Compat::Safe, format!("the {ka}'s name changed"));
                }
                if x.get("underlying") != y.get("underlying") {
                    self.add(
                        at,
                        Compat::Safe,
                        "the underlying integer changed (values are names)".into(),
                    );
                }
                let (cx, cy) = (entries(x.get("constants")), entries(y.get("constants")));
                for (n, v) in &cx {
                    match cy.get(n) {
                        None => {
                            self.add(at, Compat::NeedsMigration, format!("constant {n} removed"))
                        }
                        Some(w) if w != v => {
                            self.add(at, Compat::Safe, format!("constant {n} renumbered"));
                        }
                        Some(_) => {}
                    }
                }
                for n in cy.keys().filter(|n| !cx.contains_key(*n)) {
                    self.add(at, Compat::Safe, format!("constant {n} added"));
                }
            }
            "array" => {
                if x["count"] != y["count"] {
                    self.add(
                        at,
                        Compat::NeedsMigration,
                        format!("the array's count {:?} became {:?}", x["count"], y["count"]),
                    );
                }
                self.layout(&join(at, "[]"), &x["array"], &y["array"]);
            }
            "vector" | "option" => self.layout(&join(at, "[]"), &x[ka], &y[ka]),
            "map" => {
                if let (Some(Ipld::List(p)), Some(Ipld::List(q))) = (x.get("map"), y.get("map")) {
                    self.layout(&join(at, "{key}"), &p[0], &q[0]);
                    self.layout(&join(at, "{value}"), &p[1], &q[1]);
                }
            }
            "union" => {
                if x["union"] != y["union"] {
                    self.add(at, Compat::Safe, "the union's name changed".into());
                }
                let (vx, vy) = (entries(x.get("variants")), entries(y.get("variants")));
                for (n, pa) in &vx {
                    let p = join(at, n);
                    match vy.get(n) {
                        None => self.add(&p, Compat::NeedsMigration, "variant removed".into()),
                        Some(pb) => match (&pa[0], &pb[0]) {
                            (Ipld::Null, Ipld::Null) => {}
                            (Ipld::Null, _) | (_, Ipld::Null) => self.add(
                                &p,
                                Compat::NeedsMigration,
                                "the variant's payload was added or removed".into(),
                            ),
                            (qa, qb) => self.layout(&p, qa, qb),
                        },
                    }
                }
                for n in vy.keys().filter(|n| !vx.contains_key(*n)) {
                    self.add(&join(at, n), Compat::Safe, "variant added".into());
                }
            }
            "runtime" => self.add(
                at,
                Compat::Safe,
                "the runtime part's name changed (its value is null)".into(),
            ),
            _ => self.add(
                at,
                Compat::NeedsMigration,
                "an unknown layout changed".into(),
            ),
        }
    }

    /// The rules of `a` against `b` (either may be unrefined).
    fn rules(&mut self, at: &str, a: &Ipld, b: &Ipld) {
        let empty = BTreeMap::new();
        let rules = |l: &Ipld| -> (Ipld, BTreeMap<String, Ipld>) {
            match node_of(l) {
                Some(m) => (
                    m["logical"].clone(),
                    match m.get("rules") {
                        Some(Ipld::Map(r)) => r.clone(),
                        _ => BTreeMap::new(),
                    },
                ),
                None => (Ipld::Null, empty.clone()),
            }
        };
        let ((na, ra), (nb, rb)) = (rules(a), rules(b));
        if na != nb {
            self.add(
                at,
                Compat::Safe,
                format!("logical type {na:?} became {nb:?}"),
            );
        }
        // range
        match (ra.get("range"), rb.get("range")) {
            (None, None) => {}
            (Some(_), None) => self.add(at, Compat::Safe, "range removed".into()),
            (None, Some(r)) => self.add(at, Compat::NeedsCheck, format!("range {r:?} added")),
            (Some(Ipld::List(p)), Some(Ipld::List(q))) if p != q => {
                if looser(&p[0], &q[0], true) && looser(&p[1], &q[1], false) {
                    self.add(at, Compat::Safe, format!("range {p:?} loosened to {q:?}"));
                } else {
                    self.add(
                        at,
                        Compat::NeedsCheck,
                        format!("range {p:?} tightened to {q:?}"),
                    );
                }
            }
            _ => {}
        }
        if ra.contains_key("finite") != rb.contains_key("finite") {
            self.add(
                at,
                Compat::Safe,
                "finite added or removed (stored floats are always finite)".into(),
            );
        }
        match (ra.get("unit"), rb.get("unit")) {
            (Some(u), Some(v)) if u != v => self.add(
                at,
                Compat::NeedsMigration,
                format!("unit {u:?} became {v:?} (the values mean something else)"),
            ),
            (None, Some(_)) | (Some(_), None) => {
                self.add(at, Compat::Safe, "unit added or removed".into());
            }
            _ => {}
        }
        match (ra.get("refers_to"), rb.get("refers_to")) {
            (None, None) => {}
            (Some(_), None) => self.add(at, Compat::Safe, "refers_to removed".into()),
            (None, Some(_)) => self.add(
                at,
                Compat::NeedsCheck,
                "refers_to added (every value must be a catalog name)".into(),
            ),
            (Some(Ipld::List(p)), Some(Ipld::List(q))) => {
                if p[0] != q[0] {
                    self.add(
                        at,
                        Compat::NeedsMigration,
                        format!("catalog {:?} became {:?}", p[0], q[0]),
                    );
                } else {
                    let (old, new) = (names(&p[1]), names(&q[1]));
                    let gone: Vec<_> = old.difference(&new).cloned().collect();
                    let added: Vec<_> = new.difference(&old).cloned().collect();
                    if !gone.is_empty() {
                        self.add(
                            at,
                            Compat::NeedsMigration,
                            format!("catalog names removed: {}", gone.join(", ")),
                        );
                    }
                    if !added.is_empty() {
                        self.add(
                            at,
                            Compat::Safe,
                            format!("catalog names added: {}", added.join(", ")),
                        );
                    }
                }
            }
            _ => {}
        }
        let (ca, cb) = (
            ra.get("validated").map(names).unwrap_or_default(),
            rb.get("validated").map(names).unwrap_or_default(),
        );
        for c in cb.difference(&ca) {
            self.add(at, Compat::NeedsCheck, format!("check {c} added"));
        }
        for c in ca.difference(&cb) {
            self.add(at, Compat::Safe, format!("check {c} removed"));
        }
    }
}
