//! Merge rules as runtime data (SPEC.md, "Merging"). A rule is a target of the `Merge` relationship in the world,
//! read back here as a plain enum, written into layout records by name and hashed with the layout.
//!
//! Merging is the merge crate's non-generic `merge(rule, a, b)`, proven in Lean (lean/). [`merge_state`] and
//! [`merge_fields`] call it on CRDT states in their DAG-CBOR form (SPEC.md, "Merge states"): the state is decoded into
//! the merge crate's value model, checked to be well formed, merged, and encoded back. Nothing is merged field by field
//! here; that is the merge crate's job.

use std::collections::BTreeMap;

use ipld_core::ipld::Ipld;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    /// The write with the greatest (HLC, actor) wins.
    LastWriter,
    /// A set: an element is present if any add is not covered by a remove that saw it.
    AddWins,
    /// Concurrent increments add up.
    Counter,
    /// Concurrent writes are all kept, for the reader (or a later write) to resolve.
    KeepBoth,
    /// A text CRDT over the string.
    Text,
}

impl Rule {
    /// Never drop a concurrent write silently (SPEC.md, "Default rule").
    pub const DEFAULT: Rule = Rule::KeepBoth;

    pub const ALL: [Rule; 5] = [
        Rule::LastWriter,
        Rule::AddWins,
        Rule::Counter,
        Rule::KeepBoth,
        Rule::Text,
    ];

    /// The name used in scripts (`(Merge, LastWriter)`) and in layout records.
    pub fn name(self) -> &'static str {
        match self {
            Rule::LastWriter => "LastWriter",
            Rule::AddWins => "AddWins",
            Rule::Counter => "Counter",
            Rule::KeepBoth => "KeepBoth",
            Rule::Text => "Text",
        }
    }

    pub fn from_name(name: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|r| r.name() == name)
    }

    /// Whether the rule can apply to a value of this layout (SPEC.md, "Layouts").
    pub fn check(self, layout: &Ipld) -> Result<(), String> {
        let prim = match layout {
            Ipld::String(s) => Some(s.as_str()),
            _ => None,
        };
        let ok = match self {
            Rule::LastWriter | Rule::KeepBoth => true,
            Rule::Text => prim == Some("string"),
            Rule::Counter => matches!(
                prim,
                Some(
                    "u8" | "u16"
                        | "u32"
                        | "u64"
                        | "i8"
                        | "i16"
                        | "i32"
                        | "i64"
                        | "f32"
                        | "f64"
                        | "uptr"
                        | "iptr"
                        | "byte"
                )
            ),
            Rule::AddWins => matches!(layout, Ipld::Map(m) if m.contains_key("vector")),
        };
        if ok {
            Ok(())
        } else {
            let what = match self {
                Rule::Text => "a string",
                Rule::Counter => "a number",
                _ => "a vector",
            };
            Err(format!("(Merge, {}) needs {what}", self.name()))
        }
    }
}

impl Rule {
    /// The merge crate's rule, or an error for a rule it does not implement yet (Text).
    fn engine(self) -> Result<crdt::Rule, String> {
        Ok(match self {
            Rule::LastWriter => crdt::Rule::LastWriter,
            Rule::AddWins => crdt::Rule::AddWins,
            Rule::Counter => crdt::Rule::Counter,
            Rule::KeepBoth => crdt::Rule::KeepBoth,
            Rule::Text => return Err("(Merge, Text) has no merge function yet".into()),
        })
    }
}

type R<T> = Result<T, String>;

fn fields<'a>(v: &'a Ipld, keys: &[&str]) -> R<&'a BTreeMap<String, Ipld>> {
    let Ipld::Map(m) = v else {
        return Err(format!("expected a map with {keys:?}"));
    };
    if m.len() != keys.len() || keys.iter().any(|k| !m.contains_key(*k)) {
        let got: Vec<_> = m.keys().collect();
        return Err(format!("expected exactly the keys {keys:?}, got {got:?}"));
    }
    Ok(m)
}

fn list(v: &Ipld) -> R<&[Ipld]> {
    match v {
        Ipld::List(l) => Ok(l),
        _ => Err("expected a list".into()),
    }
}

fn uint(v: &Ipld) -> R<u64> {
    match v {
        Ipld::Integer(i) => u64::try_from(*i).map_err(|_| format!("{i} is not a u64")),
        _ => Err("expected an unsigned integer".into()),
    }
}

/// `[wall, counter, actor]`.
fn hlc(v: &Ipld) -> R<crdt::Hlc> {
    match list(v)? {
        [w, c, a] => Ok(crdt::Hlc {
            wall: uint(w)?,
            counter: u32::try_from(uint(c)?).map_err(|_| "HLC counter is not a u32")?,
            actor: uint(a)?,
        }),
        _ => Err("expected a stamp [wall, counter, actor]".into()),
    }
}

fn hlc_out(h: &crdt::Hlc) -> Ipld {
    Ipld::List(vec![
        Ipld::Integer(h.wall as i128),
        Ipld::Integer(h.counter as i128),
        Ipld::Integer(h.actor as i128),
    ])
}

fn pair(v: &Ipld) -> R<(&Ipld, &Ipld)> {
    match list(v)? {
        [a, b] => Ok((a, b)),
        _ => Err("expected a pair".into()),
    }
}

// Lists are built back to front, as the merge crate's cons lists are.
fn stamps(v: &Ipld) -> R<crdt::Stamps> {
    let mut out = crdt::Stamps::Nil;
    for x in list(v)?.iter().rev() {
        out = crdt::Stamps::Cons(hlc(x)?, Box::new(out));
    }
    Ok(out)
}

fn tagged(v: &Ipld) -> R<crdt::Tagged> {
    let mut out = crdt::Tagged::Nil;
    for x in list(v)?.iter().rev() {
        let (k, v) = pair(x)?;
        out = crdt::Tagged::Cons(hlc(k)?, uint(v)?, Box::new(out));
    }
    Ok(out)
}

fn counts(v: &Ipld) -> R<crdt::Counts> {
    let mut out = crdt::Counts::Nil;
    for x in list(v)?.iter().rev() {
        let (k, v) = pair(x)?;
        out = crdt::Counts::Cons(uint(k)?, uint(v)?, Box::new(out));
    }
    Ok(out)
}

fn stamps_out(mut s: &crdt::Stamps) -> Ipld {
    let mut out = Vec::new();
    while let crdt::Stamps::Cons(h, rest) = s {
        out.push(hlc_out(h));
        s = rest;
    }
    Ipld::List(out)
}

fn tagged_out(mut s: &crdt::Tagged) -> Ipld {
    let mut out = Vec::new();
    while let crdt::Tagged::Cons(h, v, rest) = s {
        out.push(Ipld::List(vec![hlc_out(h), Ipld::Integer(*v as i128)]));
        s = rest;
    }
    Ipld::List(out)
}

fn counts_out(mut s: &crdt::Counts) -> Ipld {
    let mut out = Vec::new();
    while let crdt::Counts::Cons(k, v, rest) = s {
        out.push(Ipld::List(vec![
            Ipld::Integer(*k as i128),
            Ipld::Integer(*v as i128),
        ]));
        s = rest;
    }
    Ipld::List(out)
}

fn tombstoned(v: &Ipld) -> R<crdt::Tombstoned> {
    let m = fields(v, &["entries", "removed"])?;
    Ok(crdt::Tombstoned {
        entries: tagged(&m["entries"])?,
        removed: stamps(&m["removed"])?,
    })
}

fn map_of(entries: Vec<(&str, Ipld)>) -> Ipld {
    Ipld::Map(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

/// A rule's state from its DAG-CBOR form (SPEC.md, "Merge states"), refused unless well formed.
fn state(rule: Rule, v: &Ipld) -> R<crdt::Value> {
    let value = match rule.engine()? {
        crdt::Rule::LastWriter => {
            let m = fields(v, &["stamp", "value"])?;
            crdt::Value::Register(crdt::Register {
                stamp: hlc(&m["stamp"])?,
                value: uint(&m["value"])?,
            })
        }
        crdt::Rule::AddWins => crdt::Value::Set(tombstoned(v)?),
        crdt::Rule::KeepBoth => crdt::Value::Multi(tombstoned(v)?),
        crdt::Rule::Counter => {
            let m = fields(v, &["inc", "dec"])?;
            crdt::Value::Counter(crdt::Pn {
                inc: counts(&m["inc"])?,
                dec: counts(&m["dec"])?,
            })
        }
    };
    if !crdt::validate(rule.engine()?, &value) {
        return Err(format!(
            "{} state is not well formed: lists must be sorted strictly by key",
            rule.name()
        ));
    }
    Ok(value)
}

fn state_out(v: &crdt::Value) -> Ipld {
    match v {
        crdt::Value::Register(r) => map_of(vec![
            ("stamp", hlc_out(&r.stamp)),
            ("value", Ipld::Integer(r.value as i128)),
        ]),
        crdt::Value::Set(t) | crdt::Value::Multi(t) => map_of(vec![
            ("entries", tagged_out(&t.entries)),
            ("removed", stamps_out(&t.removed)),
        ]),
        crdt::Value::Counter(p) => map_of(vec![
            ("inc", counts_out(&p.inc)),
            ("dec", counts_out(&p.dec)),
        ]),
    }
}

fn engine_error(e: crdt::Error) -> String {
    match e {
        crdt::Error::Mismatch => "the states do not fit the rule".into(),
        crdt::Error::Overflow => "a counter or clock would overflow".into(),
    }
}

/// Joins two states of one rule (SPEC.md, "Merge states") with the merge crate's `merge`.
pub fn merge_state(rule: Rule, a: &Ipld, b: &Ipld) -> R<Ipld> {
    let (x, y) = (state(rule, a)?, state(rule, b)?);
    crdt::merge(rule.engine()?, &x, &y)
        .map(|v| state_out(&v))
        .map_err(engine_error)
}

/// Joins two struct states member by member, each under its own rule (the merge crate's `merge_fields`). `a` and `b`
/// list the members' states in declaration order.
pub fn merge_fields(rules: &[Rule], a: &[Ipld], b: &[Ipld]) -> R<Vec<Ipld>> {
    if a.len() != rules.len() || b.len() != rules.len() {
        return Err("each struct state needs one state per member rule".into());
    }
    let mut rs = crdt::Rules::Nil;
    let (mut fa, mut fb) = (crdt::Fields::Nil, crdt::Fields::Nil);
    for i in (0..rules.len()).rev() {
        rs = crdt::Rules::Cons(rules[i].engine()?, Box::new(rs));
        fa = crdt::Fields::Cons(state(rules[i], &a[i])?, Box::new(fa));
        fb = crdt::Fields::Cons(state(rules[i], &b[i])?, Box::new(fb));
    }
    let mut f = &crdt::merge_fields(&rs, &fa, &fb).map_err(engine_error)?;
    let mut out = Vec::new();
    while let crdt::Fields::Cons(v, rest) = f {
        out.push(state_out(v));
        f = rest;
    }
    Ok(out)
}
