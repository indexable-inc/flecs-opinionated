//! Strict reading (SPEC.md, "Versioning"): every entity record and layout record must have exactly the keys the spec
//! lists, every layout hash must be the hash of its record, and every component value must fit the layout its hash
//! names. Unknown keys inside a record are refused, because they may change what the record means. Only the top level
//! of a document and of a root block may carry keys this reader does not know.
//!
//! The same value check guards `World::set`, where a blob is given as its bytes rather than as a link.

use std::collections::{BTreeMap, BTreeSet};

use ipld_core::ipld::Ipld;

use crate::dag::{DAG_CBOR, RAW, VERSION};
use crate::merge::Rule;
use crate::mode::Mode;
use crate::walk::{encode, hash8, member_err};

pub type R<T> = Result<T, String>;

/// Primitive layout names (SPEC.md, "Layouts"), with the integer range of each integer type.
fn int_range(name: &str) -> Option<(i128, i128)> {
    Some(match name {
        "char" | "i8" => (i8::MIN as i128, i8::MAX as i128),
        "u8" | "byte" => (0, u8::MAX as i128),
        "u16" => (0, u16::MAX as i128),
        "u32" => (0, u32::MAX as i128),
        "u64" | "uptr" => (0, u64::MAX as i128),
        "i16" => (i16::MIN as i128, i16::MAX as i128),
        "i32" => (i32::MIN as i128, i32::MAX as i128),
        "i64" | "iptr" => (i64::MIN as i128, i64::MAX as i128),
        _ => return None,
    })
}

const OTHER_PRIMITIVES: [&str; 8] = [
    "bool", "f32", "f64", "string", "entity", "id", "bytes", "blob",
];

/// The map's keys are exactly `keys`.
pub fn exact<'a>(v: &'a Ipld, keys: &[&str], what: &str) -> R<&'a BTreeMap<String, Ipld>> {
    let Ipld::Map(m) = v else {
        return Err(format!("{what} is not a map"));
    };
    for k in m.keys() {
        if !keys.contains(&k.as_str()) {
            return Err(format!("{what} has the unknown key {k:?}"));
        }
    }
    for k in keys {
        if !m.contains_key(*k) {
            return Err(format!("{what} has no {k:?}"));
        }
    }
    Ok(m)
}

fn list<'a>(v: &'a Ipld, what: &str) -> R<&'a [Ipld]> {
    match v {
        Ipld::List(l) => Ok(l),
        _ => Err(format!("{what} is not a list")),
    }
}

fn text<'a>(v: &'a Ipld, what: &str) -> R<&'a str> {
    match v {
        Ipld::String(s) => Ok(s),
        _ => Err(format!("{what} is not text")),
    }
}

fn int(v: &Ipld, what: &str) -> R<i128> {
    match v {
        Ipld::Integer(i) => Ok(*i),
        _ => Err(format!("{what} is not an integer")),
    }
}

fn nullable<'a, T>(v: &'a Ipld, f: impl FnOnce(&'a Ipld) -> R<T>) -> R<Option<T>> {
    match v {
        Ipld::Null => Ok(None),
        v => f(v).map(Some),
    }
}

/// `{"id": bytes16}` or `{"path": text}`.
pub fn reference(v: &Ipld, what: &str) -> R<()> {
    let Ipld::Map(m) = v else {
        return Err(format!("{what} is not a ref"));
    };
    match (m.len(), m.get("id"), m.get("path")) {
        (1, Some(Ipld::Bytes(b)), None) if b.len() == 16 => Ok(()),
        (1, None, Some(Ipld::String(_))) => Ok(()),
        _ => Err(format!(
            "{what} is not a ref ({{\"id\": 16 bytes}} or {{\"path\": text}})"
        )),
    }
}

fn rule(v: &Ipld, what: &str) -> R<Option<Rule>> {
    nullable(v, |v| {
        let n = text(v, what)?;
        Rule::from_name(n).ok_or_else(|| format!("{what}: {n:?} is not a merge rule"))
    })
}

/// A layout `L` (SPEC.md, "Layouts") of a whole component or pair value.
pub fn layout(l: &Ipld) -> R<()> {
    layout_in(l, false)
}

fn is_struct(l: &Ipld) -> bool {
    matches!(l, Ipld::Map(m) if m.contains_key("struct"))
}

/// `covered`: inside a value one rule merges whole, where no struct member carries a rule (SPEC.md, "Merge rules").
/// Outside, every member carries one, except a nested struct merged field by field (null).
fn layout_in(l: &Ipld, covered: bool) -> R<()> {
    match l {
        Ipld::String(p) if p == "uptr" || p == "iptr" => Err(format!(
            "{p} has the host's pointer width and is refused in layouts"
        )),
        Ipld::String(p) => {
            if int_range(p).is_some() || OTHER_PRIMITIVES.contains(&p.as_str()) {
                Ok(())
            } else {
                Err(format!("unknown primitive layout {p:?}"))
            }
        }
        Ipld::Map(m) => {
            if m.contains_key("struct") {
                let m = exact(l, &["struct", "members"], "struct layout")?;
                nullable(&m["struct"], |v| text(v, "struct name"))?;
                let mut names = BTreeSet::new();
                for x in list(&m["members"], "members")? {
                    let [n, ml, r] = list(x, "member")? else {
                        return Err("a member is not [name, layout, rule]".into());
                    };
                    if !names.insert(text(n, "member name")?) {
                        return Err(format!("member {n:?} is listed twice"));
                    }
                    let r = rule(r, "member rule")?;
                    let n = text(n, "member name")?;
                    match (covered, r) {
                        (true, Some(_)) => {
                            return Err(format!(
                                "member {n}: has a rule inside a value merged whole by an enclosing rule"
                            ));
                        }
                        (false, None) if !is_struct(ml) => {
                            return Err(format!("member {n}: has no merge rule"));
                        }
                        _ => {}
                    }
                    layout_in(ml, covered || r.is_some()).map_err(|e| member_err(n, e))?;
                }
                Ok(())
            } else if m.contains_key("enum") {
                let m = exact(l, &["enum", "underlying", "constants"], "enum layout")?;
                nullable(&m["enum"], |v| text(v, "enum name"))?;
                match &m["underlying"] {
                    Ipld::String(u) if int_range(u).is_some() => {}
                    _ => return Err("an enum's underlying layout is not an integer type".into()),
                }
                constants(&m["constants"])
            } else if m.contains_key("bitmask") {
                let m = exact(l, &["bitmask", "constants"], "bitmask layout")?;
                nullable(&m["bitmask"], |v| text(v, "bitmask name"))?;
                constants(&m["constants"])
            } else if m.contains_key("array") {
                let m = exact(l, &["array", "count"], "array layout")?;
                if int(&m["count"], "array count")? < 0 {
                    return Err("negative array count".into());
                }
                layout_in(&m["array"], true)
            } else if m.contains_key("vector") {
                layout_in(&exact(l, &["vector"], "vector layout")?["vector"], true)
            } else {
                Err(format!(
                    "unknown layout kind with keys {:?}",
                    m.keys().collect::<Vec<_>>()
                ))
            }
        }
        _ => Err("a layout is neither a primitive name nor a map".into()),
    }
}

fn constants(v: &Ipld) -> R<()> {
    for c in list(v, "constants")? {
        let [n, x] = list(c, "constant")? else {
            return Err("a constant is not [name, value]".into());
        };
        text(n, "constant name")?;
        int(x, "constant value")?;
    }
    Ok(())
}

/// A layout record (SPEC.md, "Layouts"), whose hash must be the first 8 bytes of SHA-256 over the record without it.
pub fn layout_record(path: &str, rec: &Ipld) -> R<()> {
    let what = format!("layout {path}");
    let m = exact(
        rec,
        &[
            "layout",
            "merge",
            "sync",
            "writable_by",
            "validated_by",
            "hash",
        ],
        &what,
    )?;
    let merge = rule(&m["merge"], &format!("{what}: merge"))?;
    match (is_struct(&m["layout"]), merge) {
        (true, Some(_)) => {
            return Err(format!(
                "{what}: a struct's rules are on its members, so merge must be null"
            ));
        }
        (false, None) => return Err(format!("{what}: has no merge rule")),
        _ => {}
    }
    layout_in(&m["layout"], merge.is_some()).map_err(|e| format!("{what}: {e}"))?;
    let mode = text(&m["sync"], &format!("{what}: sync"))?;
    Mode::from_name(mode).ok_or_else(|| format!("{what}: {mode:?} is not a sync mode"))?;
    nullable(&m["writable_by"], |v| reference(v, "writable_by"))?;
    nullable(&m["validated_by"], |v| reference(v, "validated_by"))?;
    let mut body = m.clone();
    let hash = body.remove("hash");
    if hash != Some(Ipld::Bytes(hash8(&encode(&Ipld::Map(body))))) {
        return Err(format!(
            "{what}: its hash is not the hash of the record (the record was changed, or written by another version)"
        ));
    }
    Ok(())
}

/// Where a blob value comes from: a stored record holds a link, `World::set` takes the bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Blobs {
    Links,
    Bytes,
}

/// A value against its layout: the same shapes `walk` writes (SPEC.md, "Values"), maps with exactly the members.
pub fn value(v: &Ipld, l: &Ipld, blobs: Blobs) -> R<()> {
    match l {
        Ipld::String(p) => {
            let p = p.as_str();
            if let Some((lo, hi)) = int_range(p) {
                let i = int(v, p)?;
                return if (lo..=hi).contains(&i) {
                    Ok(())
                } else {
                    Err(format!("{i} does not fit {p}"))
                };
            }
            let ok = match (p, v) {
                ("bool", Ipld::Bool(_)) => true,
                ("f32" | "f64", Ipld::Float(f)) => f.is_finite(),
                ("string", Ipld::String(_) | Ipld::Null) => true,
                ("entity", Ipld::Null) | ("id", Ipld::Null) => true,
                ("entity", r) => return reference(r, "entity value"),
                ("id", Ipld::List(pair)) if pair.len() == 2 => {
                    reference(&pair[0], "relationship")?;
                    return reference(&pair[1], "target");
                }
                ("id", r) => return reference(r, "id value"),
                ("bytes", Ipld::Bytes(_)) => true,
                ("blob", Ipld::Link(c)) => {
                    blobs == Blobs::Links && [RAW, DAG_CBOR].contains(&c.codec())
                }
                ("blob", Ipld::Bytes(_)) => blobs == Blobs::Bytes,
                _ => false,
            };
            if ok {
                Ok(())
            } else {
                Err(format!("{v:?} is not a {p} value"))
            }
        }
        Ipld::Map(m) if m.contains_key("struct") => {
            let Ipld::Map(vm) = v else {
                return Err("a struct value is not a map".into());
            };
            let members = list(&m["members"], "members")?;
            for k in vm.keys() {
                let known = members.iter().any(
                    |x| matches!(x, Ipld::List(x) if x.first() == Some(&Ipld::String(k.clone()))),
                );
                if !known {
                    return Err(format!("the struct value has the unknown member {k:?}"));
                }
            }
            for x in members {
                let [Ipld::String(n), ml, _] = list(x, "member")? else {
                    return Err("bad member".into());
                };
                let mv = vm
                    .get(n)
                    .ok_or_else(|| format!("the struct value has no member {n:?}"))?;
                value(mv, ml, blobs).map_err(|e| member_err(n, e))?;
            }
            Ok(())
        }
        Ipld::Map(m) if m.contains_key("enum") => {
            let n = text(v, "enum value")?;
            if names(&m["constants"])?.contains(n) {
                Ok(())
            } else {
                Err(format!("{n:?} is not a constant of the enum"))
            }
        }
        Ipld::Map(m) if m.contains_key("bitmask") => {
            let known = names(&m["constants"])?;
            let mut seen = BTreeSet::new();
            for x in list(v, "bitmask value")? {
                let n = text(x, "bitmask constant")?;
                if !known.contains(n) || !seen.insert(n) {
                    return Err(format!(
                        "{n:?} is not a constant of the bitmask, or is listed twice"
                    ));
                }
            }
            Ok(())
        }
        Ipld::Map(m) if m.contains_key("array") => {
            let items = list(v, "array value")?;
            if items.len() as i128 != int(&m["count"], "count")? {
                return Err(format!("the array value has {} elements", items.len()));
            }
            items.iter().enumerate().try_for_each(|(i, x)| {
                value(x, &m["array"], blobs).map_err(|e| member_err(&format!("[{i}]"), e))
            })
        }
        Ipld::Map(m) if m.contains_key("vector") => list(v, "vector value")?
            .iter()
            .enumerate()
            .try_for_each(|(i, x)| {
                value(x, &m["vector"], blobs).map_err(|e| member_err(&format!("[{i}]"), e))
            }),
        _ => Err("unknown layout".into()),
    }
}

fn names(v: &Ipld) -> R<BTreeSet<&str>> {
    list(v, "constants")?
        .iter()
        .map(|c| match c {
            Ipld::List(c) => text(&c[0], "constant name"),
            _ => Err("bad constant".into()),
        })
        .collect()
}

/// The layouts map of a document or root block, each record checked.
pub fn layouts(v: Option<&Ipld>) -> R<&BTreeMap<String, Ipld>> {
    let Some(Ipld::Map(m)) = v else {
        return Err("no layouts map".into());
    };
    for (path, rec) in m {
        layout_record(path, rec)?;
    }
    Ok(m)
}

/// `{"hash", "value"}` (plus `"type"` for pair data) against the layout record its type names.
fn typed(v: &Ipld, ty: &str, layouts: &BTreeMap<String, Ipld>, keys: &[&str]) -> R<()> {
    let m = exact(v, keys, &format!("value of {ty}"))?;
    let rec = layouts
        .get(ty)
        .ok_or_else(|| format!("{ty} has no layout record"))?;
    let Ipld::Map(rec) = rec else {
        return Err(format!("layout {ty} is not a map"));
    };
    if rec.get("hash") != Some(&m["hash"]) {
        return Err(format!(
            "the value of {ty} names a layout hash it does not have"
        ));
    }
    value(&m["value"], &rec["layout"], Blobs::Links).map_err(|e| format!("{ty}: {e}"))
}

/// The form an entity record comes in.
#[derive(Clone, Copy)]
pub enum Form {
    /// A document entity: the record plus `path` and `parent`.
    Document,
    /// An entity block: the record plus `children`.
    Block,
}

/// An entity record (SPEC.md, "Entity record") in one of its two forms.
pub fn entity(v: &Ipld, form: Form, layouts: &BTreeMap<String, Ipld>) -> R<()> {
    const BASE: [&str; 7] = [
        "id",
        "name",
        "tags",
        "pairs",
        "components",
        "isa",
        "inherited",
    ];
    let mut keys = BASE.to_vec();
    match form {
        Form::Document => keys.extend(["path", "parent"]),
        Form::Block => keys.push("children"),
    }
    let m = exact(v, &keys, "entity record")?;
    match &m["id"] {
        Ipld::Null => {}
        Ipld::Bytes(b) if b.len() == 16 => {}
        _ => return Err("entity id is not 16 bytes or null".into()),
    }
    nullable(&m["name"], |v| text(v, "name"))?;
    for t in list(&m["tags"], "tags")? {
        reference(t, "tag")?;
    }
    for p in list(&m["pairs"], "pairs")? {
        match list(p, "pair")? {
            [r, t] => {
                reference(r, "relationship")?;
                reference(t, "target")?;
            }
            [r, t, data] => {
                reference(r, "relationship")?;
                reference(t, "target")?;
                let Some(Ipld::String(ty)) = (match data {
                    Ipld::Map(d) => d.get("type"),
                    _ => None,
                }) else {
                    return Err("pair data has no type".into());
                };
                typed(data, ty, layouts, &["type", "hash", "value"])?;
            }
            _ => return Err("a pair is not [rel, tgt] or [rel, tgt, data]".into()),
        }
    }
    let Ipld::Map(cs) = &m["components"] else {
        return Err("components is not a map".into());
    };
    for (ty, c) in cs {
        typed(c, ty, layouts, &["hash", "value"])?;
    }
    for r in list(&m["isa"], "isa")? {
        reference(r, "isa")?;
    }
    let inh = exact(
        &m["inherited"],
        &["tags", "components", "pairs"],
        "inherited",
    )?;
    for t in list(&inh["tags"], "inherited tags")? {
        reference(t, "inherited tag")?;
    }
    for c in list(&inh["components"], "inherited components")? {
        text(c, "inherited component")?;
    }
    for p in list(&inh["pairs"], "inherited pairs")? {
        let [r, t] = list(p, "inherited pair")? else {
            return Err("an inherited pair is not [rel, tgt]".into());
        };
        reference(r, "relationship")?;
        reference(t, "target")?;
    }
    match form {
        Form::Document => {
            text(&m["path"], "path")?;
            nullable(&m["parent"], |v| int(v, "parent"))?;
        }
        Form::Block => {
            for c in list(&m["children"], "children")? {
                match list(c, "child")? {
                    [Ipld::String(_), Ipld::Link(l)] if l.codec() == DAG_CBOR => {}
                    _ => return Err("a child is not [segment, dag-cbor link]".into()),
                }
            }
        }
    }
    Ok(())
}

pub fn version(m: &BTreeMap<String, Ipld>, what: &str) -> R<()> {
    match m.get("version") {
        Some(Ipld::Integer(v)) if *v == VERSION as i128 => Ok(()),
        Some(Ipld::Integer(v)) => Err(format!(
            "format version {v} is not supported (this reader knows {VERSION})"
        )),
        _ => Err(format!("{what} has no format version")),
    }
}

/// A whole document: the version, every layout record and every entity record.
pub fn document(doc: &Ipld) -> R<()> {
    let Ipld::Map(m) = doc else {
        return Err("the document is not a map".into());
    };
    version(m, "the document")?;
    let ls = layouts(m.get("layouts"))?;
    let es = list(
        m.get("entities").ok_or("the document has no entities")?,
        "entities",
    )?;
    for (i, e) in es.iter().enumerate() {
        entity(e, Form::Document, ls).map_err(|err| format!("entity {i}: {err}"))?;
    }
    Ok(())
}
