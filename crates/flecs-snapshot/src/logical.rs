//! Logical types (SPEC.md, "Logical types"): a physical layout refined by rules. The physical layout alone decides a
//! value's encoding and its Flecs meta; the rules say which of those values are valid. A refinement is the layout node
//! `{"logical": name | null, "of": L, "rules": {rule: argument}}`, made from Flecs data: a logical type is an entity
//! that is `(IsA, <physical type>)` and carries rule components (`sync.Range`, `sync.Finite`, `(sync.Unit, u)`,
//! `(sync.RefersTo, catalog)`, `(sync.Validated, check)`), and a struct member carries the same rules or is
//! `(IsA, <logical type>)`. Described types write the node in their description and get the entities made from it.
//!
//! Rules (the keys of `rules`, each at most once):
//! - `"range": [min | null, max | null]`: an integer or float physical type; the bounds are integers for an integer
//!   type and floats for a float one, inclusive, null where unbounded;
//! - `"finite": null`: a float physical type (DAG-CBOR already refuses NaN and the infinities, so it adds no check
//!   to a stored value; it is kept so the layout says what the type promises);
//! - `"unit": path`: an integer or float physical type; the unit's entity path (`flecs.units.Length.Meters`);
//! - `"refers_to": [catalog path, [names]]`: a string physical type, the value one of the names (sorted bytewise,
//!   unique), which are the children of the catalog entity;
//! - `"validated": [check paths]`: any physical type; each check (an entity with `sync.Check`, its function given by the
//!   host process) must accept the value before a restore or a write takes it.

use std::collections::BTreeMap;
use std::ffi::CString;

use flecs_c as sys;
use ipld_core::ipld::Ipld;
use sys::{ecs_entity_t, ecs_world_t};

use crate::check::{int_range, is_float};

pub type R<T> = Result<T, String>;

/// The rule names, in the order they are kept (bytewise, as DAG-CBOR map keys of equal length sort; checked below).
pub const RULES: [&str; 5] = ["finite", "range", "refers_to", "unit", "validated"];

/// A check's function: accepts a value in its snapshot shape (SPEC.md, "Values") or says why not.
pub type CheckFn = fn(&Ipld) -> R<()>;

/// The check every world has (`sync.checks.NonZero`): an integer that is not 0 (a signed `NonZero` has a hole in the
/// middle of its range, which a `range` rule cannot say).
pub const NONZERO_CHECK: &str = "sync.checks.NonZero";

fn nonzero(v: &Ipld) -> R<()> {
    match v {
        Ipld::Integer(0) => Err("0 is not a nonzero integer".into()),
        Ipld::Integer(_) => Ok(()),
        _ => Err("not an integer".into()),
    }
}

/// The logical node's map, when `l` is one.
pub fn node_of(l: &Ipld) -> Option<&BTreeMap<String, Ipld>> {
    match l {
        Ipld::Map(m) if m.contains_key("logical") => Some(m),
        _ => None,
    }
}

/// The physical layout under a logical node (the layout itself when it is not one).
pub fn physical(l: &Ipld) -> &Ipld {
    match node_of(l).and_then(|m| m.get("of")) {
        Some(of) => physical(of),
        None => l,
    }
}

/// Whether a layout has a logical node anywhere.
pub fn refined(l: &Ipld) -> bool {
    match l {
        Ipld::Map(m) => m.contains_key("logical") || m.values().any(refined),
        Ipld::List(xs) => xs.iter().any(refined),
        _ => false,
    }
}

/// Whether a layout has a `validated` rule anywhere.
pub fn has_checks(l: &Ipld) -> bool {
    match l {
        Ipld::Map(m) => {
            matches!(m.get("rules"), Some(Ipld::Map(r)) if m.contains_key("logical") && r.contains_key("validated"))
                || m.values().any(has_checks)
        }
        Ipld::List(xs) => xs.iter().any(has_checks),
        _ => false,
    }
}

/// What kind of number a physical layout holds: `Some(false)` an integer, `Some(true)` a float.
fn number(of: &Ipld) -> Option<bool> {
    match of {
        Ipld::String(p) if int_range(p).is_some() => Some(false),
        Ipld::String(p) if is_float(p) => Some(true),
        _ => None,
    }
}

fn prim(of: &Ipld) -> &str {
    match of {
        Ipld::String(p) => p,
        _ => "a composite layout",
    }
}

/// The order of two bounds of one kind (both integers or both floats).
fn cmp(a: &Ipld, b: &Ipld) -> std::cmp::Ordering {
    match (a, b) {
        (Ipld::Integer(x), Ipld::Integer(y)) => x.cmp(y),
        (Ipld::Float(x), Ipld::Float(y)) => x.total_cmp(y),
        (Ipld::Integer(x), Ipld::Float(y)) => (*x as f64).total_cmp(y),
        (Ipld::Float(x), Ipld::Integer(y)) => x.total_cmp(&(*y as f64)),
        _ => std::cmp::Ordering::Equal,
    }
}

fn show(v: &Ipld) -> String {
    match v {
        Ipld::Integer(i) => i.to_string(),
        Ipld::Float(f) => format!("{f:?}"),
        Ipld::String(s) => format!("{s:?}"),
        Ipld::Null => "null".into(),
        v => format!("{v:?}"),
    }
}

fn texts(v: &Ipld, what: &str) -> R<Vec<String>> {
    let Ipld::List(xs) = v else {
        return Err(format!("{what} is not a list"));
    };
    let out = xs
        .iter()
        .map(|x| match x {
            Ipld::String(s) => Ok(s.clone()),
            _ => Err(format!("{what} holds something that is not text")),
        })
        .collect::<R<Vec<_>>>()?;
    if out.windows(2).any(|w| w[0] >= w[1]) {
        return Err(format!("{what} is not sorted bytewise without repeats"));
    }
    Ok(out)
}

fn texts_ipld(xs: Vec<String>) -> Ipld {
    Ipld::List(xs.into_iter().map(Ipld::String).collect())
}

/// A bound as the physical type keeps it: an integer for an integer type (a float bound must be a whole number that
/// fits), a float for a float type.
fn bound(v: &Ipld, of: &Ipld) -> R<Ipld> {
    let float =
        number(of).ok_or_else(|| format!("range on {}, which is not a number", prim(of)))?;
    match (v, float) {
        (Ipld::Null, _) => Ok(Ipld::Null),
        (Ipld::Integer(i), true) => Ok(Ipld::Float(*i as f64)),
        (Ipld::Float(f), true) if f.is_finite() => Ok(Ipld::Float(*f)),
        (Ipld::Float(f), false) if f.is_finite() && f.fract() == 0.0 && f.abs() < 2e38 => {
            Ok(Ipld::Integer(*f as i128))
        }
        (Ipld::Integer(i), false) => Ok(Ipld::Integer(*i)),
        _ => Err(format!(
            "the range bound {} is not a {} bound",
            show(v),
            prim(of)
        )),
    }
}

/// Normalises a range for `of`: bounds of the right kind, clipped to the physical type's own range, null where the
/// clip leaves nothing to say. `None` when nothing is left (the rule says nothing the physical type does not).
fn range(arg: &Ipld, of: &Ipld) -> R<Option<Ipld>> {
    let Ipld::List(b) = arg else {
        return Err("range is not [min, max]".into());
    };
    let [lo, hi] = b.as_slice() else {
        return Err("range is not [min, max]".into());
    };
    let (mut lo, mut hi) = (bound(lo, of)?, bound(hi, of)?);
    if let Ipld::String(p) = of
        && let Some((min, max)) = int_range(p)
    {
        if matches!(lo, Ipld::Integer(i) if i <= min) {
            lo = Ipld::Null;
        }
        if matches!(hi, Ipld::Integer(i) if i >= max) {
            hi = Ipld::Null;
        }
    }
    if !lo.eq(&Ipld::Null) && !hi.eq(&Ipld::Null) && cmp(&lo, &hi).is_gt() {
        return Err(format!("the range [{}, {}] is empty", show(&lo), show(&hi)));
    }
    Ok((lo != Ipld::Null || hi != Ipld::Null).then(|| Ipld::List(vec![lo, hi])))
}

/// One rule's argument, normalised and checked against the physical layout `of`. `None`: the rule says nothing.
fn rule(name: &str, arg: &Ipld, of: &Ipld) -> R<Option<Ipld>> {
    match name {
        "range" => range(arg, of),
        "finite" => {
            if number(of) != Some(true) {
                return Err(format!("finite on {}, which is not a float", prim(of)));
            }
            match arg {
                Ipld::Null => Ok(Some(Ipld::Null)),
                _ => Err("finite takes no argument (null)".into()),
            }
        }
        "unit" => {
            if number(of).is_none() {
                return Err(format!("unit on {}, which is not a number", prim(of)));
            }
            match arg {
                Ipld::String(s) if !s.is_empty() => Ok(Some(arg.clone())),
                _ => Err("a unit is not an entity path".into()),
            }
        }
        "refers_to" => {
            if !matches!(of, Ipld::String(p) if p == "string") {
                return Err(format!("refers_to on {}, which is not a string", prim(of)));
            }
            let Ipld::List(x) = arg else {
                return Err("refers_to is not [catalog, names]".into());
            };
            let [Ipld::String(c), names] = x.as_slice() else {
                return Err("refers_to is not [catalog, names]".into());
            };
            if c.is_empty() {
                return Err("refers_to names no catalog".into());
            }
            texts(names, "refers_to's names")?;
            Ok(Some(arg.clone()))
        }
        "validated" => {
            let checks = texts(arg, "validated")?;
            if checks.is_empty() || checks.iter().any(String::is_empty) {
                return Err("validated names no check".into());
            }
            Ok(Some(arg.clone()))
        }
        _ => Err(format!(
            "unknown rule {name:?} (one of {})",
            RULES.join(", ")
        )),
    }
}

/// Two rule sets on one physical type as one: ranges intersect, `finite` holds if either says it, checks add up;
/// two units or two catalogs that differ are refused (a value cannot be in both).
pub fn merge_rules(
    a: &BTreeMap<String, Ipld>,
    b: &BTreeMap<String, Ipld>,
) -> R<BTreeMap<String, Ipld>> {
    let mut out = a.clone();
    for (k, y) in b {
        let Some(x) = a.get(k) else {
            out.insert(k.clone(), y.clone());
            continue;
        };
        let v = match k.as_str() {
            "range" => {
                let (Ipld::List(x), Ipld::List(y)) = (x, y) else {
                    return Err("range is not [min, max]".into());
                };
                let pick = |p: &Ipld, q: &Ipld, lower: bool| match (p, q) {
                    (Ipld::Null, q) => q.clone(),
                    (p, Ipld::Null) => p.clone(),
                    (p, q) => {
                        if cmp(p, q).is_gt() == lower {
                            p.clone()
                        } else {
                            q.clone()
                        }
                    }
                };
                Ipld::List(vec![pick(&x[0], &y[0], true), pick(&x[1], &y[1], false)])
            }
            "finite" => Ipld::Null,
            "validated" => {
                let mut all = texts(x, "validated")?;
                all.extend(texts(y, "validated")?);
                all.sort();
                all.dedup();
                texts_ipld(all)
            }
            "unit" | "refers_to" if x == y => x.clone(),
            "unit" => {
                return Err(format!("two units, {} and {}", show(x), show(y)));
            }
            "refers_to" => return Err("two catalogs".into()),
            k => return Err(format!("unknown rule {k:?}")),
        };
        out.insert(k.clone(), v);
    }
    Ok(out)
}

/// A logical node over `of`, normalised: a logical `of` is folded in (its rules merged with these, `name` kept when
/// given, else its name), every rule checked against the physical layout. Without a name and with no rule left the
/// result is `of` itself, so an unrefined layout stays byte-identical.
pub fn node(name: Option<String>, of: Ipld, rules: BTreeMap<String, Ipld>) -> R<Ipld> {
    let (name, of, rules) = match node_of(&of) {
        Some(inner) => {
            let iname = match &inner["logical"] {
                Ipld::String(s) => Some(s.clone()),
                _ => None,
            };
            let Some(Ipld::Map(ir)) = inner.get("rules") else {
                return Err("a logical node has no rules map".into());
            };
            let merged = merge_rules(ir, &rules)?;
            (name.or(iname), inner["of"].clone(), merged)
        }
        None => (name, of, rules),
    };
    let mut out = BTreeMap::new();
    for (k, v) in &rules {
        if let Some(v) = rule(k, v, &of).map_err(|e| match &name {
            Some(n) => format!("{n}: {e}"),
            None => e,
        })? {
            out.insert(k.clone(), v);
        }
    }
    if name.is_none() && out.is_empty() {
        return Ok(of);
    }
    Ok(Ipld::Map(BTreeMap::from([
        ("logical".to_string(), name.map_or(Ipld::Null, Ipld::String)),
        ("of".to_string(), of),
        ("rules".to_string(), Ipld::Map(out)),
    ])))
}

/// A logical node as a reader takes it: the keys, a name or null, an `of` that is not itself logical, and rules in
/// their normal form (re-normalising them must change nothing). `of` is checked by the caller.
pub fn check_node(m: &BTreeMap<String, Ipld>) -> R<()> {
    for k in m.keys() {
        if !["logical", "of", "rules"].contains(&k.as_str()) {
            return Err(format!("logical layout has the unknown key {k:?}"));
        }
    }
    let (Some(name), Some(of), Some(rules)) = (m.get("logical"), m.get("of"), m.get("rules"))
    else {
        return Err("a logical layout is not {logical, of, rules}".into());
    };
    if !matches!(name, Ipld::Null | Ipld::String(_)) {
        return Err("a logical layout's name is not text or null".into());
    }
    if node_of(of).is_some() {
        return Err("a logical layout's of is itself logical (they fold into one)".into());
    }
    let Ipld::Map(rules) = rules else {
        return Err("a logical layout's rules are not a map".into());
    };
    for (k, v) in rules {
        match rule(k, v, of)? {
            Some(n) if n == *v => {}
            _ => {
                return Err(format!("rule {k}: {} is not in normal form", show(v)));
            }
        }
    }
    if matches!(name, Ipld::Null) && rules.is_empty() {
        return Err("a logical layout with no name and no rule (write its of)".into());
    }
    Ok(())
}

fn named(m: &BTreeMap<String, Ipld>) -> String {
    match &m["logical"] {
        Ipld::String(n) => format!(" ({n})"),
        _ => String::new(),
    }
}

/// A value against a logical node's rules, its physical shape already checked: range and catalog membership
/// (`validated` needs a world, see [`run_checks`]).
pub fn check_value(v: &Ipld, m: &BTreeMap<String, Ipld>) -> R<()> {
    let Some(Ipld::Map(rules)) = m.get("rules") else {
        return Ok(());
    };
    if let Some(Ipld::List(b)) = rules.get("range") {
        if !b[0].eq(&Ipld::Null) && cmp(v, &b[0]).is_lt() {
            return Err(format!(
                "{} is below the minimum {}{}",
                show(v),
                show(&b[0]),
                named(m)
            ));
        }
        if !b[1].eq(&Ipld::Null) && cmp(v, &b[1]).is_gt() {
            return Err(format!(
                "{} is above the maximum {}{}",
                show(v),
                show(&b[1]),
                named(m)
            ));
        }
    }
    if let Ipld::Float(f) = v
        && rules.contains_key("finite")
        && !f.is_finite()
    {
        return Err(format!("{f} is not finite{}", named(m)));
    }
    if let Some(Ipld::List(x)) = rules.get("refers_to") {
        let (Ipld::String(cat), Ipld::List(names)) = (&x[0], &x[1]) else {
            return Err("bad refers_to".into());
        };
        let ok = match v {
            Ipld::String(_) => names.binary_search_by(|n| match (n, v) {
                (Ipld::String(a), Ipld::String(b)) => a.as_str().cmp(b.as_str()),
                _ => std::cmp::Ordering::Less,
            }),
            _ => Err(0),
        };
        if ok.is_err() {
            return Err(format!(
                "{} is not a name in the catalog {cat}{}",
                show(v),
                named(m)
            ));
        }
    }
    Ok(())
}

// ---- Flecs data ------------------------------------------------------------------------------------------------------

/// The sync module's logical vocabulary in one world.
#[derive(Clone, Copy)]
pub struct Vocab {
    pub range: ecs_entity_t,
    pub finite: ecs_entity_t,
    pub unit: ecs_entity_t,
    pub refers_to: ecs_entity_t,
    pub catalog: ecs_entity_t,
    pub validated: ecs_entity_t,
    pub check: ecs_entity_t,
}

fn lookup_path(w: *const ecs_world_t, path: &str) -> ecs_entity_t {
    crate::host::lookup(w, path).unwrap_or(0)
}

impl Vocab {
    pub fn find(w: *const ecs_world_t) -> R<Vocab> {
        let get = |p: &str| match lookup_path(w, p) {
            0 => Err(format!("sync module: {p} missing")),
            e => Ok(e),
        };
        Ok(Vocab {
            range: get("sync.Range")?,
            finite: get("sync.Finite")?,
            unit: get("sync.Unit")?,
            refers_to: get("sync.RefersTo")?,
            catalog: get("sync.Catalog")?,
            validated: get("sync.Validated")?,
            check: get("sync.Check")?,
        })
    }
}

/// Gives `sync.Range` its constructor (unbounded: `-f64::MAX` to `f64::MAX`, finite so a snapshot can hold it), so a
/// script writes only the bound it means (`sync.Range: {min: 1}`). Run once, right after the sync module's script.
pub(crate) fn init(w: *mut ecs_world_t) -> R<()> {
    let range = lookup_path(w, "sync.Range");
    // SAFETY: `range` is the sync module's Range component, a struct of two f64s.
    if range == 0 || unsafe { sys::glue_range_hooks(w, range) } != 0 {
        return Err("sync module: cannot give sync.Range its constructor".into());
    }
    // SAFETY: the caller's world is live.
    unsafe { register_check(w, NONZERO_CHECK, nonzero) }.map(|_| ())
}

/// The entity at `path`, made (as plain named entities) where a segment is missing.
fn ensure_path(w: *mut ecs_world_t, path: &str) -> R<ecs_entity_t> {
    let mut e = 0;
    for seg in crate::host::segments(path)? {
        let c = CString::new(seg).map_err(|_| format!("path {path:?} holds a NUL byte"))?;
        // SAFETY: a lookup of a child by exact name, made when missing.
        e = unsafe { sys::glue_describe_type(w, e, c.as_ptr(), 0, 0) };
        if e == 0 {
            return Err(format!("cannot make {path}"));
        }
    }
    Ok(e)
}

/// Registers a check's function at `path` (an entity with `sync.Check`, made when missing), so a `validated` rule
/// naming it runs it. Registering again (a live patch) replaces the function.
///
/// # Safety
///
/// `world` is live and not used by another thread during the call; it has the sync module (or gets it).
pub unsafe fn register_check(world: *mut ecs_world_t, path: &str, f: CheckFn) -> R<ecs_entity_t> {
    crate::schema::ensure(world)?;
    let comp = lookup_path(world, "sync.Check");
    if comp == 0 {
        return Err("sync.Check missing".into());
    }
    let e = ensure_path(world, path)?;
    let p = f as usize;
    // SAFETY: sync.Check is `{ptr: uptr}`, one pointer-sized integer.
    unsafe {
        sys::ecs_set_id(
            world,
            e,
            comp,
            core::mem::size_of::<usize>(),
            (&raw const p).cast(),
        )
    };
    Ok(e)
}

fn check_fn(w: *const ecs_world_t, v: &Vocab, path: &str) -> R<CheckFn> {
    let e = lookup_path(w, path);
    // SAFETY: plain queries; sync.Check holds a function pointer `register_check` stored, valid for the process.
    unsafe {
        if e == 0 || !sys::ecs_owns_id(w, e, v.check) {
            return Err(format!(
                "the check {path} is not registered in this world (register_check)"
            ));
        }
        let p = *sys::ecs_get_id(w, e, v.check).cast::<usize>();
        if p == 0 {
            return Err(format!("the check {path} has no function"));
        }
        Ok(core::mem::transmute::<usize, CheckFn>(p))
    }
}

/// Runs every `validated` check in layout `l` on value `v` (already checked against `l`), the member path in the error.
pub fn run_checks(w: *const ecs_world_t, v: &Ipld, l: &Ipld) -> R<()> {
    if !has_checks(l) {
        return Ok(());
    }
    let vocab = Vocab::find(w)?;
    walk_checks(w, &vocab, v, l)
}

fn walk_checks(w: *const ecs_world_t, vocab: &Vocab, v: &Ipld, l: &Ipld) -> R<()> {
    use crate::walk::member_err;
    let Ipld::Map(m) = l else {
        return Ok(());
    };
    if m.contains_key("logical") {
        walk_checks(w, vocab, v, &m["of"])?;
        if let Some(Ipld::Map(r)) = m.get("rules")
            && let Some(cs) = r.get("validated")
        {
            for c in texts(cs, "validated")? {
                check_fn(w, vocab, &c)?(v).map_err(|e| format!("{c} refused it: {e}"))?;
            }
        }
        return Ok(());
    }
    if !has_checks(l) {
        return Ok(());
    }
    let items = |v: &Ipld| match v {
        Ipld::List(xs) => xs.clone(),
        _ => Vec::new(),
    };
    if let (Some(Ipld::List(ms)), Ipld::Map(vm)) = (m.get("members"), v) {
        for x in ms {
            if let Ipld::List(x) = x
                && let (Some(Ipld::String(n)), Some(ml)) = (x.first(), x.get(1))
                && let Some(mv) = vm.get(n)
            {
                walk_checks(w, vocab, mv, ml).map_err(|e| member_err(n, e))?;
            }
        }
    } else if let Some(el) = m.get("array").or(m.get("vector")).or(m.get("option")) {
        for (i, x) in items(v).iter().enumerate() {
            walk_checks(w, vocab, x, el).map_err(|e| member_err(&format!("[{i}]"), e))?;
        }
    } else if let Some(Ipld::List(kv)) = m.get("map") {
        for (i, e) in items(v).iter().enumerate() {
            if let Ipld::List(e) = e {
                walk_checks(w, vocab, &e[0], &kv[0])
                    .map_err(|x| member_err(&format!("[{i}].key"), x))?;
                walk_checks(w, vocab, &e[1], &kv[1])
                    .map_err(|x| member_err(&format!("[{i}].value"), x))?;
            }
        }
    } else if let (Some(Ipld::List(vs)), Ipld::Map(vm)) = (m.get("variants"), v)
        && let Some((n, pv)) = vm.iter().next()
    {
        for var in vs {
            if let Ipld::List(var) = var
                && var.first() == Some(&Ipld::String(n.clone()))
                && let Some(pl) = var.get(1)
            {
                walk_checks(w, vocab, pv, pl).map_err(|e| member_err(n, e))?;
            }
        }
    }
    Ok(())
}

/// The rules an entity carries itself (not through IsA): `sync.Range`, `sync.Finite`, `(sync.Unit, u)`,
/// `(sync.RefersTo, catalog)` with the catalog's children as its names, `(sync.Validated, check)`. Bounds are floats
/// here; [`node`] turns them into the physical type's kind. `path` names an entity for a ref.
pub(crate) fn own_rules(
    w: *const ecs_world_t,
    v: &Vocab,
    e: ecs_entity_t,
    path: &mut dyn FnMut(ecs_entity_t) -> R<String>,
) -> R<BTreeMap<String, Ipld>> {
    let mut out = BTreeMap::new();
    // SAFETY: plain queries on a live world; Range is two f64s.
    unsafe {
        if sys::ecs_owns_id(w, e, v.range) {
            let r = *sys::ecs_get_id(w, e, v.range).cast::<[f64; 2]>();
            let b = |x: f64| {
                if x.abs() >= f64::MAX {
                    Ipld::Null
                } else {
                    Ipld::Float(x)
                }
            };
            out.insert("range".into(), Ipld::List(vec![b(r[0]), b(r[1])]));
        }
        if sys::ecs_owns_id(w, e, v.finite) {
            out.insert("finite".into(), Ipld::Null);
        }
    }
    let mut checks = Vec::new();
    let mut i = 0;
    loop {
        // SAFETY: plain query.
        let t = unsafe { sys::ecs_get_target(w, e, v.validated, i) };
        if t == 0 {
            break;
        }
        // SAFETY: plain query. Only its own: a base's checks come with the base's rules.
        if unsafe { sys::ecs_owns_id(w, e, sys::pair(v.validated, t)) } {
            checks.push(path(t)?);
        }
        i += 1;
    }
    if !checks.is_empty() {
        checks.sort();
        checks.dedup();
        out.insert("validated".into(), texts_ipld(checks));
    }
    // SAFETY: plain queries.
    let (unit, cat) = unsafe { (own_target(w, e, v.unit), own_target(w, e, v.refers_to)) };
    if unit != 0 {
        out.insert("unit".into(), Ipld::String(path(unit)?));
    }
    if cat != 0 {
        let cp = path(cat)?;
        // SAFETY: plain query.
        if !unsafe { sys::ecs_has_id(w, cat, v.catalog) } {
            return Err(format!("(RefersTo, {cp}): {cp} is not a sync.Catalog"));
        }
        let mut names: Vec<String> = crate::world::grow(|buf, cap| {
            // SAFETY: `buf` has room for `cap` ids.
            unsafe { sys::glue_children(w, cat, buf, cap) }
        })
        .into_iter()
        .filter_map(|c| {
            // SAFETY: plain query; Flecs names are NUL-terminated.
            let n = unsafe { sys::ecs_get_name(w, c) };
            (!n.is_null()).then(|| {
                unsafe { std::ffi::CStr::from_ptr(n) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect();
        names.sort();
        names.dedup();
        out.insert(
            "refers_to".into(),
            Ipld::List(vec![Ipld::String(cp), texts_ipld(names)]),
        );
    }
    Ok(out)
}

/// The target of an exclusive relationship `rel` that `e` owns (not inherited), or 0.
unsafe fn own_target(w: *const ecs_world_t, e: ecs_entity_t, rel: ecs_entity_t) -> ecs_entity_t {
    // SAFETY: plain queries.
    unsafe {
        let t = sys::ecs_get_target(w, e, rel, 0);
        if t != 0 && sys::ecs_owns_id(w, e, sys::pair(rel, t)) {
            t
        } else {
            0
        }
    }
}

/// Makes the Flecs data a described layout's logical nodes stand for: each named logical type as an entity under
/// `sync.types` (`(IsA, <physical type>)` when Flecs has the primitive, and its rule components), each catalog
/// (`sync.Catalog` with a child per name) and a check for each `validated` rule (which must be registered already).
/// A unit is linked only when the world has its entity. An existing logical type entity is given these rules again.
///
/// # Safety
///
/// `world` is live and not used by another thread during the call; it has the sync module.
pub unsafe fn materialise(world: *mut ecs_world_t, l: &Ipld) -> R<()> {
    if !refined(l) {
        return Ok(());
    }
    let v = Vocab::find(world)?;
    let mut nodes = Vec::new();
    collect(l, &mut nodes);
    for m in nodes {
        let Some(Ipld::Map(rules)) = m.get("rules") else {
            continue;
        };
        if let Some(Ipld::List(x)) = rules.get("refers_to")
            && let [Ipld::String(cat), Ipld::List(names)] = x.as_slice()
        {
            let c = ensure_path(world, cat)?;
            // SAFETY: plain writes on a live world.
            unsafe { sys::ecs_add_id(world, c, v.catalog) };
            for n in names {
                if let Ipld::String(n) = n {
                    ensure_path(world, &format!("{cat}.{}", crate::walk::escape(n)))?;
                }
            }
        }
        if let Some(cs) = rules.get("validated") {
            for c in texts(cs, "validated")? {
                check_fn(world, &v, &c)?;
            }
        }
        let Ipld::String(name) = &m["logical"] else {
            continue;
        };
        let e = ensure_path(world, &format!("sync.types.{}", crate::walk::escape(name)))?;
        set_rules(world, &v, e, &m["of"], rules)?;
    }
    Ok(())
}

fn collect<'a>(l: &'a Ipld, out: &mut Vec<&'a BTreeMap<String, Ipld>>) {
    match l {
        Ipld::Map(m) => {
            if m.contains_key("logical") {
                out.push(m);
            }
            m.values().for_each(|x| collect(x, out));
        }
        Ipld::List(xs) => xs.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

/// Puts `rules` on entity `e` as components and pairs (replacing the ones it had), plus `(IsA, <physical primitive>)`.
fn set_rules(
    w: *mut ecs_world_t,
    v: &Vocab,
    e: ecs_entity_t,
    of: &Ipld,
    rules: &BTreeMap<String, Ipld>,
) -> R<()> {
    // SAFETY: plain reads and writes on a live world; Range is two f64s.
    unsafe {
        if let Ipld::String(p) = of {
            let prim = lookup_path(w, &format!("flecs.meta.{p}"));
            if prim != 0 {
                sys::ecs_add_id(w, e, sys::pair(sys::EcsIsA, prim));
            }
        }
        match rules.get("range") {
            Some(Ipld::List(b)) => {
                let f = |x: &Ipld, unbounded: f64| match x {
                    Ipld::Integer(i) => *i as f64,
                    Ipld::Float(f) => *f,
                    _ => unbounded,
                };
                let r = [f(&b[0], -f64::MAX), f(&b[1], f64::MAX)];
                sys::ecs_set_id(w, e, v.range, 16, r.as_ptr().cast());
            }
            _ => sys::ecs_remove_id(w, e, v.range),
        }
        if rules.contains_key("finite") {
            sys::ecs_add_id(w, e, v.finite);
        } else {
            sys::ecs_remove_id(w, e, v.finite);
        }
        for rel in [v.unit, v.refers_to, v.validated] {
            sys::ecs_remove_id(w, e, sys::pair(rel, sys::EcsWildcard));
        }
        if let Some(Ipld::String(u)) = rules.get("unit") {
            let t = lookup_path(w, u);
            if t != 0 {
                sys::ecs_add_id(w, e, sys::pair(v.unit, t));
            }
        }
        if let Some(Ipld::List(x)) = rules.get("refers_to")
            && let Some(Ipld::String(cat)) = x.first()
        {
            sys::ecs_add_id(w, e, sys::pair(v.refers_to, lookup_path(w, cat)));
        }
        if let Some(cs) = rules.get("validated") {
            for c in texts(cs, "validated")? {
                sys::ecs_add_id(w, e, sys::pair(v.validated, lookup_path(w, &c)));
            }
        }
    }
    Ok(())
}

/// For Flecs's reflection (REST, the explorer): puts a struct member's rules on its member entity (the logical type
/// as `(IsA, type)` when it has a name, else the rules themselves) and mirrors the range and unit into Flecs's own
/// `EcsMemberRanges` and `EcsMember.unit`, for each member of `ty`'s Flecs struct whose layout in `l` is logical,
/// nested structs included. Flecs does not enforce either; the library does, on save, restore and `set`.
///
/// # Safety
///
/// As [`materialise`], which ran first for `l`.
pub unsafe fn annotate_members(world: *mut ecs_world_t, ty: ecs_entity_t, l: &Ipld) -> R<()> {
    let Some(Ipld::List(ms)) = (match physical(l) {
        Ipld::Map(m) if m.contains_key("struct") => m.get("members"),
        _ => None,
    }) else {
        return Ok(());
    };
    if !refined(l) {
        return Ok(());
    }
    let v = Vocab::find(world)?;
    for x in ms {
        let Ipld::List(x) = x else { continue };
        let (Some(Ipld::String(n)), Some(ml)) = (x.first(), x.get(1)) else {
            continue;
        };
        let Ok(c) = CString::new(n.as_str()) else {
            continue;
        };
        // SAFETY: plain lookups; member entities are the struct's children by name.
        let me = unsafe { sys::ecs_lookup_child(world, ty, c.as_ptr()) };
        // SAFETY: as above.
        let mt = if me == 0 {
            0
        } else {
            unsafe { sys::glue_member_type(world, me) }
        };
        if mt == 0 {
            continue;
        }
        if let Some(m) = node_of(ml)
            && let Some(Ipld::Map(rules)) = m.get("rules")
        {
            match &m["logical"] {
                Ipld::String(name) => {
                    let t =
                        lookup_path(world, &format!("sync.types.{}", crate::walk::escape(name)));
                    if t != 0 {
                        // SAFETY: plain write.
                        unsafe { sys::ecs_add_id(world, me, sys::pair(sys::EcsIsA, t)) };
                    }
                }
                _ => set_rules(world, &v, me, &Ipld::Null, rules)?,
            }
            let (has, lo, hi) = match rules.get("range") {
                Some(Ipld::List(b)) => {
                    let f = |x: &Ipld, d: f64| match x {
                        Ipld::Integer(i) => *i as f64,
                        Ipld::Float(f) => *f,
                        _ => d,
                    };
                    (true, f(&b[0], -f64::MAX), f(&b[1], f64::MAX))
                }
                _ => (false, 0.0, 0.0),
            };
            let unit = match rules.get("unit") {
                Some(Ipld::String(u)) => lookup_path(world, u),
                _ => 0,
            };
            // SAFETY: `me` is a member entity of a live struct.
            unsafe { sys::glue_member_refine(world, me, has, lo, hi, unit) };
        }
        unsafe { annotate_members(world, mt, ml) }?;
    }
    Ok(())
}
