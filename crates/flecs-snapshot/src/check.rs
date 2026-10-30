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
pub(crate) fn int_range(name: &str) -> Option<(i128, i128)> {
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

const OTHER_PRIMITIVES: [&str; 10] = [
    "bool", "f32", "f64", "f16", "bf16", "string", "entity", "id", "bytes", "blob",
];

/// The float layouts (SPEC.md, "Values"; `f16` and `bf16` come from described types).
pub(crate) fn is_float(p: &str) -> bool {
    matches!(p, "f32" | "f64" | "f16" | "bf16")
}

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

/// As `exact`, with `optional` keys that may also be there.
pub fn exact_opt<'a>(
    v: &'a Ipld,
    keys: &[&str],
    optional: &[&str],
    what: &str,
) -> R<&'a BTreeMap<String, Ipld>> {
    let Ipld::Map(m) = v else {
        return Err(format!("{what} is not a map"));
    };
    for k in m.keys() {
        if !keys.contains(&k.as_str()) && !optional.contains(&k.as_str()) {
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

/// A runtime layout's body: `{"size": n, "align": n, "implicit": bool}` (size >= 0, align a power of two), or the
/// text of a record written before (a Rust path, read but never written).
fn runtime_layout(r: &Ipld) -> R<()> {
    if let Ipld::String(_) = r {
        return Ok(());
    }
    let m = exact(r, &["size", "align", "implicit"], "runtime layout's body").map_err(|e| {
        format!("{e} (a runtime layout is {{\"runtime\": {{\"size\", \"align\", \"implicit\"}}}})")
    })?;
    let size = int(&m["size"], "runtime size")?;
    let align = int(&m["align"], "runtime align")?;
    if size < 0 || align < 1 || align.count_ones() != 1 || size % align != 0 {
        return Err(format!(
            "runtime size {size} and align {align}: the size is not a multiple of a power-of-two align"
        ));
    }
    match &m["implicit"] {
        Ipld::Bool(_) => Ok(()),
        _ => Err("runtime implicit is not a bool".into()),
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
    layout_in(l, false, false)
}

/// A described type's layout (SPEC.md, "Described types"): well formed, but a member's rule slot may be null where
/// the library fills in the default.
pub fn described_layout(l: &Ipld) -> R<()> {
    layout_in(l, false, true)
}

/// A described layout with its rule slots filled as for a Flecs struct (SPEC.md, "Described types"): a member's own
/// rule, else `whole` (the type's `(Merge, rule)`), else null for a nested struct and the default for anything else;
/// inside a covered value (a member with a rule, an element, an option's value, a union's payload, a map's keys and
/// values) every slot stays null, and a slot set there is an error.
pub fn fill_rules(l: &Ipld, covered: bool, whole: Option<Rule>) -> R<Ipld> {
    let Ipld::Map(m) = l else {
        return Ok(l.clone());
    };
    let mut out = m.clone();
    if m.contains_key("logical") {
        // A refinement changes nothing about merging: its physical layout's slots are filled.
        out.insert("of".into(), fill_rules(&m["of"], covered, whole)?);
        return Ok(Ipld::Map(out));
    }
    if m.contains_key("struct") {
        let mut members = Vec::new();
        for x in list(&m["members"], "members")? {
            let [n, ml, r] = list(x, "member")? else {
                return Err("a member is not [name, layout, rule]".into());
            };
            let name = text(n, "member name")?;
            let own = rule(r, "member rule")?;
            if covered && own.is_some() {
                return Err(format!(
                    "member {name}: has a rule inside a value merged whole by an enclosing rule"
                ));
            }
            let rule = if covered {
                None
            } else {
                match own.or(whole) {
                    Some(r) => Some(r),
                    None if is_struct(ml) => None,
                    None => Some(Rule::DEFAULT),
                }
            };
            let inner =
                fill_rules(ml, covered || rule.is_some(), None).map_err(|e| member_err(name, e))?;
            if let Some(r) = rule {
                r.check(&inner).map_err(|e| member_err(name, e))?;
            }
            members.push(Ipld::List(vec![
                n.clone(),
                inner,
                rule.map_or(Ipld::Null, |r| Ipld::String(r.name().into())),
            ]));
        }
        out.insert("members".into(), Ipld::List(members));
        return Ok(Ipld::Map(out));
    }
    for k in ["array", "vector", "option"] {
        if let Some(inner) = m.get(k) {
            out.insert(k.into(), fill_rules(inner, true, None)?);
            return Ok(Ipld::Map(out));
        }
    }
    if let Some(Ipld::List(kv)) = m.get("map") {
        let kv = kv
            .iter()
            .map(|x| fill_rules(x, true, None))
            .collect::<R<Vec<_>>>()?;
        out.insert("map".into(), Ipld::List(kv));
        return Ok(Ipld::Map(out));
    }
    if m.contains_key("union") {
        let mut vs = Vec::new();
        for x in list(&m["variants"], "variants")? {
            let [n, pl] = list(x, "variant")? else {
                return Err("a variant is not [name, layout | null]".into());
            };
            let name = text(n, "variant name")?;
            let pl = match pl {
                Ipld::Null => Ipld::Null,
                pl => fill_rules(pl, true, None).map_err(|e| member_err(name, e))?,
            };
            vs.push(Ipld::List(vec![n.clone(), pl]));
        }
        out.insert("variants".into(), Ipld::List(vs));
    }
    Ok(Ipld::Map(out))
}

/// Whether a layout is a struct, under any refinement (SPEC.md, "Logical types": a refinement does not change how a
/// value merges).
pub fn is_struct(l: &Ipld) -> bool {
    matches!(crate::logical::physical(l), Ipld::Map(m) if m.contains_key("struct"))
}

/// `covered`: inside a value one rule merges whole, where no struct member carries a rule (SPEC.md, "Merge rules").
/// Outside, every member carries one, except a nested struct merged field by field (null). `lenient` allows a null
/// slot anywhere (a described layout, before `fill_rules`).
fn layout_in(l: &Ipld, covered: bool, lenient: bool) -> R<()> {
    match l {
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
                        (false, None) if !is_struct(ml) && !lenient => {
                            return Err(format!("member {n}: has no merge rule"));
                        }
                        _ => {}
                    }
                    layout_in(ml, covered || r.is_some(), lenient).map_err(|e| member_err(n, e))?;
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
                layout_in(&m["array"], true, lenient)
            } else if m.contains_key("vector") {
                layout_in(
                    &exact(l, &["vector"], "vector layout")?["vector"],
                    true,
                    lenient,
                )
            } else if m.contains_key("option") {
                layout_in(
                    &exact(l, &["option"], "option layout")?["option"],
                    true,
                    lenient,
                )
            } else if m.contains_key("map") {
                let [k, v] = list(&exact(l, &["map"], "map layout")?["map"], "map layout")? else {
                    return Err("a map layout is not [key, value]".into());
                };
                layout_in(k, true, lenient).map_err(|e| member_err("key", e))?;
                layout_in(v, true, lenient).map_err(|e| member_err("value", e))
            } else if m.contains_key("union") {
                let m = exact(l, &["union", "variants"], "union layout")?;
                nullable(&m["union"], |v| text(v, "union name"))?;
                let mut names = BTreeSet::new();
                for x in list(&m["variants"], "variants")? {
                    let [n, pl] = list(x, "variant")? else {
                        return Err("a variant is not [name, layout | null]".into());
                    };
                    let n = text(n, "variant name")?;
                    if !names.insert(n) {
                        return Err(format!("variant {n:?} is listed twice"));
                    }
                    if !matches!(pl, Ipld::Null) {
                        if !is_struct(pl) {
                            return Err(format!(
                                "variant {n}: its payload is not a struct or null"
                            ));
                        }
                        layout_in(pl, true, lenient).map_err(|e| member_err(n, e))?;
                    }
                }
                Ok(())
            } else if m.contains_key("logical") {
                crate::logical::check_node(m)?;
                layout_in(&m["of"], covered, lenient)
            } else if m.contains_key("runtime") {
                runtime_layout(&exact(l, &["runtime"], "runtime layout")?["runtime"])
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

/// The base and version a type's path gives (SPEC.md, "Layouts"): a last segment ending in `V` and a decimal number
/// not starting with 0 is that version of the path without the suffix; any other path is its own base at version 1.
/// `FooV1` is refused (a second name for version 1 of `Foo`).
pub fn base_version(path: &str) -> R<(String, u64)> {
    // The last segment starts after the last `.` that is not escaped.
    let bytes = path.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'.' => {
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    let last = &path[start..];
    let digits = last.bytes().rev().take_while(u8::is_ascii_digit).count();
    let stem = &last[..last.len() - digits];
    if digits == 0
        || !stem.ends_with('V')
        || stem.len() == 1
        || last[last.len() - digits..].starts_with('0')
    {
        return Ok((path.to_string(), 1));
    }
    let version: u64 = last[last.len() - digits..]
        .parse()
        .map_err(|_| format!("type {path}: its version number is too large"))?;
    if version == 1 {
        return Err(format!(
            "type {path}: V1 is not a version suffix (version 1 of a type is the type without one)"
        ));
    }
    Ok((
        format!("{}{}", &path[..start], &stem[..stem.len() - 1]),
        version,
    ))
}

/// A layout record (SPEC.md, "Layouts"), whose hash must be the first 8 bytes of SHA-256 over the record without it.
pub fn layout_record(path: &str, rec: &Ipld) -> R<()> {
    let what = format!("layout {path}");
    let m = exact(
        rec,
        &[
            "base",
            "version",
            "layout",
            "merge",
            "sync",
            "writable_by",
            "validated_by",
            "hash",
        ],
        &what,
    )?;
    let (base, version) = base_version(path)?;
    if m["base"] != Ipld::String(base.clone()) || m["version"] != Ipld::Integer(version as i128) {
        return Err(format!(
            "{what}: base and version must be {base:?} and {version}, which its path gives"
        ));
    }
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
    layout_in(&m["layout"], merge.is_some(), false).map_err(|e| format!("{what}: {e}"))?;
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
                (p, Ipld::Float(f)) if is_float(p) => f.is_finite(),
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
        Ipld::Map(m) if m.contains_key("option") => match list(v, "option value")? {
            [] => Ok(()),
            [x] => value(x, &m["option"], blobs),
            _ => Err("an option value has more than one element".into()),
        },
        Ipld::Map(m) if m.contains_key("union") => {
            let Ipld::Map(vm) = v else {
                return Err("a union value is not a map".into());
            };
            let mut it = vm.iter();
            let (Some((n, x)), None) = (it.next(), it.next()) else {
                return Err("a union value does not have exactly one variant".into());
            };
            for var in list(&m["variants"], "variants")? {
                if let [Ipld::String(vn), pl] = list(var, "variant")?
                    && vn == n
                {
                    return match (pl, x) {
                        (Ipld::Null, Ipld::Null) => Ok(()),
                        (Ipld::Null, _) => Err(format!(
                            "variant {n} has no payload, but the value gives one"
                        )),
                        (pl, x) => value(x, pl, blobs).map_err(|e| member_err(n, e)),
                    };
                }
            }
            Err(format!("{n:?} is not a variant of the union"))
        }
        Ipld::Map(m) if m.contains_key("map") => {
            let Some(Ipld::List(kv)) = m.get("map") else {
                return Err("bad map layout".into());
            };
            let mut last: Option<Vec<u8>> = None;
            for (i, e) in list(v, "map value")?.iter().enumerate() {
                let [k, x] = list(e, "map entry")? else {
                    return Err(format!("map entry {i} is not [key, value]"));
                };
                value(k, &kv[0], blobs).map_err(|e| member_err(&format!("[{i}].key"), e))?;
                value(x, &kv[1], blobs).map_err(|e| member_err(&format!("[{i}].value"), e))?;
                let b = encode(k);
                if last.as_ref().is_some_and(|l| *l >= b) {
                    return Err(format!(
                        "map entry {i}: keys are not unique and sorted by their DAG-CBOR encoding"
                    ));
                }
                last = Some(b);
            }
            Ok(())
        }
        Ipld::Map(m) if m.contains_key("runtime") => match v {
            Ipld::Null => Ok(()),
            _ => Err("a runtime value is not null".into()),
        },
        Ipld::Map(m) if m.contains_key("logical") => {
            value(v, &m["of"], blobs)?;
            crate::logical::check_value(v, m)
        }
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
    parts(m, layouts)?;
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
        Form::Block => match &m["children"] {
            Ipld::Map(_) => {
                let c = exact(&m["children"], &["mst"], "children")?;
                match &c["mst"] {
                    Ipld::Link(l) if l.codec() == DAG_CBOR => {}
                    _ => return Err("children's mst is not a dag-cbor link".into()),
                }
            }
            v => {
                let kids = list(v, "children")?;
                if kids.len() > crate::walk::MAX_CHILDREN {
                    return Err(format!(
                        "an inline child list has {} children; past {} it must be a Merkle Search Tree",
                        kids.len(),
                        crate::walk::MAX_CHILDREN
                    ));
                }
                for c in kids {
                    match list(c, "child")? {
                        [Ipld::String(_), Ipld::Link(l)] if l.codec() == DAG_CBOR => {}
                        _ => return Err("a child is not [segment, dag-cbor link]".into()),
                    }
                }
            }
        },
    }
    Ok(())
}

/// The parts every entity record has (tags, pairs, components, isa), checked against the layouts.
fn parts(m: &BTreeMap<String, Ipld>, layouts: &BTreeMap<String, Ipld>) -> R<()> {
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
    Ok(())
}

/// The 16 bytes of an `{"id"}` ref, or None for a `{"path"}` one.
fn id_of(r: &Ipld) -> Option<[u8; 16]> {
    match r {
        Ipld::Map(m) => match m.get("id") {
            Some(Ipld::Bytes(b)) => b.as_slice().try_into().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// A host record (SPEC.md, "Host record"); returns its id. `children` and `parent_component` are optional; how they
/// match the other records is `host_document`'s check.
pub fn host_entity(v: &Ipld, layouts: &BTreeMap<String, Ipld>) -> R<[u8; 16]> {
    let m = exact_opt(
        v,
        &["id", "name", "parent", "tags", "pairs", "components", "isa"],
        &["children", "parent_component"],
        "host record",
    )?;
    let id: [u8; 16] = match &m["id"] {
        Ipld::Bytes(b) => b
            .as_slice()
            .try_into()
            .map_err(|_| "host record id is not 16 bytes")?,
        _ => return Err("host record id is not 16 bytes".into()),
    };
    nullable(&m["name"], |v| text(v, "name"))?;
    nullable(&m["parent"], |v| reference(v, "parent"))?;
    if let Some(pc) = m.get("parent_component") {
        if *pc != Ipld::Bool(true) {
            return Err("parent_component is not true (it is left out otherwise)".into());
        }
        if m["parent"] == Ipld::Null {
            return Err("parent_component is set but there is no parent".into());
        }
    }
    if let Some(c) = m.get("children") {
        let kids = list(c, "children")?;
        if kids.is_empty() {
            return Err("children is empty (it is left out when there are none)".into());
        }
        for k in kids {
            reference(k, "child")?;
            if id_of(k).is_none() {
                return Err("a child is not an {\"id\"} ref".into());
            }
        }
    }
    parts(m, layouts)?;
    Ok(id)
}

/// The child order across a host document's records (SPEC.md, "Host record"): each `children` lists exactly the
/// records whose parent it is, each once, and a record with a child in `Parent` storage lists them.
fn host_children(es: &[Ipld]) -> R<()> {
    fn rec(e: &Ipld) -> &BTreeMap<String, Ipld> {
        match e {
            Ipld::Map(m) => m,
            _ => unreachable!("checked by host_entity"),
        }
    }
    let ids: Vec<[u8; 16]> = es
        .iter()
        .map(|e| match &rec(e)["id"] {
            Ipld::Bytes(b) => b.as_slice().try_into().ok(),
            _ => None,
        })
        .collect::<Option<_>>()
        .ok_or("a host record id is not 16 bytes")?;
    let index: BTreeMap<[u8; 16], usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut kids_of: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, e) in es.iter().enumerate() {
        if let Some(p) = id_of(&rec(e)["parent"]).and_then(|id| index.get(&id)) {
            kids_of.entry(*p).or_default().push(i);
        }
    }
    for (i, e) in es.iter().enumerate() {
        let mine = kids_of.get(&i).map_or(&[][..], Vec::as_slice);
        match rec(e).get("children") {
            Some(c) => {
                let mut got = Vec::new();
                for k in list(c, "children")? {
                    let j = id_of(k).and_then(|id| index.get(&id)).ok_or_else(|| {
                        format!("entity {i}: its children list an entity that is not in the document")
                    })?;
                    got.push(*j);
                }
                let listed = got.len();
                got.sort_unstable();
                got.dedup();
                let mut want = mine.to_vec();
                want.sort_unstable();
                if got.len() != listed || got != want {
                    return Err(format!(
                        "entity {i}: its children are not exactly the entities whose parent it is, each once"
                    ));
                }
            }
            None if mine.iter().any(|&j| rec(&es[j]).contains_key("parent_component")) => {
                return Err(format!(
                    "entity {i}: a child of it is in Parent storage, but its record lists no child order"
                ));
            }
            None => {}
        }
    }
    Ok(())
}

/// A host document (SPEC.md, "Host document"): the version, the id component's path, every layout and every host
/// record, entities strictly increasing by id.
pub fn host_document(doc: &Ipld) -> R<()> {
    let m = exact(
        doc,
        &["version", "ids", "layouts", "entities"],
        "host document",
    )?;
    version(m, "the host document")?;
    text(&m["ids"], "ids")?;
    let ls = layouts(m.get("layouts"))?;
    let mut last: Option<[u8; 16]> = None;
    let es = list(&m["entities"], "entities")?;
    for (i, e) in es.iter().enumerate() {
        let id = host_entity(e, ls).map_err(|err| format!("entity {i}: {err}"))?;
        if last.is_some_and(|l| l >= id) {
            return Err(format!(
                "entity {i}: entities are not in increasing id order"
            ));
        }
        last = Some(id);
    }
    host_children(es)
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
