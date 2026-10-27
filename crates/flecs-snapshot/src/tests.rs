use ipld_core::ipld::Ipld;

use crate::{Kind, MemoryStore, World, diff};

const SITE: &str = include_str!("../../../test/scenes/site.flecs");

fn world(code: &str) -> World {
    let mut w = World::new().unwrap();
    w.run("site.flecs", code).unwrap();
    w
}

fn field<'a>(v: &'a Ipld, k: &str) -> &'a Ipld {
    match v {
        Ipld::Map(m) => m.get(k).unwrap_or_else(|| panic!("no {k} in {v:?}")),
        _ => panic!("not a map: {v:?}"),
    }
}

#[test]
fn site_document() {
    let w = world(SITE);
    let doc = w.document(&["site", "people"]).unwrap();
    let Ipld::List(es) = field(&doc, "entities") else {
        panic!()
    };
    let paths: Vec<_> = es.iter().map(|e| field(e, "path").clone()).collect();
    let want = [
        "site",
        "site.zeta",
        "site.alpha",
        "site.alpha.notes",
        "people",
        "people.alice",
        "people.bob",
    ];
    assert_eq!(paths, want.map(|p| Ipld::String(p.into())));
    // Member rule overrides the component rule; the component rule covers the rest.
    let card = field(field(&doc, "layouts"), "Card");
    let Ipld::List(members) = field(field(card, "layout"), "members") else {
        panic!()
    };
    let rules: Vec<_> = members
        .iter()
        .map(|m| {
            if let Ipld::List(x) = m {
                x[2].clone()
            } else {
                panic!()
            }
        })
        .collect();
    assert_eq!(
        rules,
        vec![
            Ipld::String("LastWriter".into()),
            Ipld::String("Counter".into()),
            Ipld::String("LastWriter".into()),
            Ipld::String("LastWriter".into()),
            Ipld::String("LastWriter".into())
        ]
    );
    let title = field(field(&doc, "layouts"), "Title");
    let Ipld::List(tm) = field(field(title, "layout"), "members") else {
        panic!()
    };
    assert_eq!(
        tm[0],
        Ipld::List(vec![
            Ipld::String("text".into()),
            Ipld::String("string".into()),
            Ipld::String("Text".into())
        ])
    );
}

#[test]
fn deterministic_and_order_independent() {
    let a = world(SITE).blocks(&["site", "people"]).unwrap();
    let b = world(SITE).blocks(&["site", "people"]).unwrap();
    assert_eq!(a.car(), b.car());
    let x = world("people {\n alice {}\n bob {}\n}\n")
        .blocks(&["people"])
        .unwrap();
    let y = world("people {\n bob {}\n alice {}\n}\n")
        .blocks(&["people"])
        .unwrap();
    assert_eq!(x.root, y.root);
}

#[test]
fn diff_finds_changed_entities() {
    let base = "struct P(v: i32)\nroot {\n a { P: {1} }\n b { P: {2} }\n c { d {} }\n}\n";
    let next = "struct P(v: i32)\nroot {\n a { P: {1} }\n b { P: {3} }\n c { e {} }\n}\n";
    let (x, y) = (
        world(base).blocks(&["root"]).unwrap(),
        world(next).blocks(&["root"]).unwrap(),
    );
    let mut s = MemoryStore::default();
    x.store(&mut s);
    y.store(&mut s);
    let d = diff(&s, &x.root, &y.root).unwrap();
    let got: Vec<_> = d
        .iter()
        .map(|d| (d.path.as_str(), d.kind.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("root.b", Kind::Changed),
            ("root.c.d", Kind::Removed),
            ("root.c.e", Kind::Added)
        ]
    );
}

#[test]
fn error_has_line() {
    let mut w = World::new().unwrap();
    let e = w
        .run("bad.flecs", "a {}\nb {\n  Nope: {1}\n}\n")
        .unwrap_err();
    assert!(e.starts_with("bad.flecs: 3: "), "{e}");
}

#[test]
fn non_finite_floats_are_refused() {
    // Flecs script cannot write NaN (0.0 / 0.0 is a script error), but C code can.
    assert!(crate::walk::float(f64::NAN).is_err());
    assert!(crate::walk::float(f64::INFINITY).is_err());
    assert!(crate::walk::float(-0.0).is_ok());
}

fn doc_entities(doc: &Ipld) -> Vec<&Ipld> {
    match field(doc, "entities") {
        Ipld::List(es) => es.iter().collect(),
        _ => panic!(),
    }
}

fn entity<'a>(doc: &'a Ipld, path: &str) -> &'a Ipld {
    doc_entities(doc)
        .into_iter()
        .find(|e| field(e, "path") == &Ipld::String(path.into()))
        .unwrap_or_else(|| panic!("no entity {path}"))
}

fn s(v: &str) -> Ipld {
    Ipld::String(v.into())
}

/// C1: an unsigned enum's constants keep their values (they were all written as 0).
#[test]
fn unsigned_enum_constants() {
    let w = world(
        "using flecs.meta\nenum U(A: 1, B: 7, {underlying_type: u32})\ne { U: {B} }\n",
    );
    let doc = w.document(&["e"]).unwrap();
    let l = field(field(field(&doc, "layouts"), "U"), "layout");
    let pair = |n: &str, v: i128| Ipld::List(vec![s(n), Ipld::Integer(v)]);
    assert_eq!(
        field(l, "constants"),
        &Ipld::List(vec![pair("A", 1), pair("B", 7)])
    );
    assert_eq!(field(l, "underlying"), &s("u32"));
    let value = field(field(field(entity(&doc, "e"), "components"), "U"), "value");
    assert_eq!(value, &s("B"));
}

/// C2: a raw value that is no constant of a signed enum is an error (0 used to match the first constant).
#[test]
fn signed_enum_raw_zero_is_refused() {
    let w = world(
        "using flecs.meta\nenum S {\n  A { (constant, i32): {5} }\n  B { (constant, i32): {6} }\n}\ne { S }\n",
    );
    let e = w.document(&["e"]).unwrap_err();
    assert!(e.contains("value 0 is not a constant of enum S"), "{e}");
}

/// C3: bases are listed a whole layer at a time, each layer by path.
#[test]
fn isa_layers_sorted_by_path() {
    let w = world("prefab Z {}\nprefab Y {}\nprefab A : Z {}\nprefab B : Y {}\ne : A, B {}\n");
    let doc = w.document(&["e", "A", "B", "Y", "Z"]).unwrap();
    let id_of = |p: &str| field(entity(&doc, p), "id").clone();
    let Ipld::List(isa) = field(entity(&doc, "e"), "isa") else {
        panic!()
    };
    let got: Vec<_> = isa.iter().map(|r| field(r, "id").clone()).collect();
    assert_eq!(got, ["A", "B", "Y", "Z"].map(id_of));
}

/// C5: two entities with one stable id are refused, by run (assign_ids) and by the walk.
#[test]
fn duplicate_stable_ids_are_refused() {
    let mut w = World::new().unwrap();
    let e = w
        .run(
            "dup.flecs",
            "a { sync.Id: {hi: 1, lo: 2} }\nb { sync.Id: {hi: 1, lo: 2} }\n",
        )
        .unwrap_err();
    assert_eq!(e, "a and b have the same stable id");
    let e = w.document(&["a", "b"]).unwrap_err();
    assert!(e.contains("have the same stable id"), "{e}");
}

/// C6: a child named `#0` and the first unnamed child get different paths, and both resolve.
#[test]
fn hash_names_do_not_collide_with_unnamed_children() {
    // A script cannot name an entity `#0` (Flecs reads that as an id), so the name is set through the C API.
    let mut w = world("struct P(v: i32)\np {\n  q { P: {1} }\n  _ { P: {2} }\n}\n");
    w.rename("p.q", "#0").unwrap();
    let doc = w.document(&["p"]).unwrap();
    let paths: Vec<_> = doc_entities(&doc)
        .iter()
        .map(|e| field(e, "path").clone())
        .collect();
    assert_eq!(paths, vec![s("p"), s("p.#0"), s("p.\\#0")]);
    w.set(
        "p.\\#0",
        "P",
        &Ipld::Map([("v".to_string(), Ipld::Integer(10))].into()),
    )
    .unwrap();
    w.set(
        "p.#0",
        "P",
        &Ipld::Map([("v".to_string(), Ipld::Integer(20))].into()),
    )
    .unwrap();
    let doc = w.document(&["p"]).unwrap();
    let v = |p: &str| {
        field(
            field(field(field(entity(&doc, p), "components"), "P"), "value"),
            "v",
        )
        .clone()
    };
    assert_eq!(v("p.\\#0"), Ipld::Integer(10));
    assert_eq!(v("p.#0"), Ipld::Integer(20));
}

/// D1: an integer literal past u64 is refused (the pinned Flecs, vendor/flecs/SOURCE.md, checks ERANGE; wasm-libc's
/// strtoull sets it as native libcs do, and bun's native test compares the outcomes). u64::MAX itself is kept.
#[test]
fn integer_literal_past_u64_is_refused() {
    let script = |lit: &str| format!("struct U(v: u64)\nx {{ U: {{{lit}}} }}\n");
    let mut w = World::new().unwrap();
    let e = w.run("int.flecs", &script("99999999999999999999")).unwrap_err();
    assert!(e.contains("out of range"), "{e}");
    let doc = world(&script("18446744073709551615")).document(&["x"]).unwrap();
    let v = field(
        field(field(field(entity(&doc, "x"), "components"), "U"), "value"),
        "v",
    );
    assert_eq!(v, &Ipld::Integer(u64::MAX as i128));
}

/// C7: a member's rule covers its whole value, so the nested struct's members carry none.
#[test]
fn nested_struct_under_a_rule_is_covered() {
    let doc = world(SITE).document(&["site"]).unwrap();
    let card = field(field(field(&doc, "layouts"), "Card"), "layout");
    let Ipld::List(ms) = field(card, "members") else {
        panic!()
    };
    let Ipld::List(pos) = &ms[0] else { panic!() };
    let Ipld::List(inner) = field(&pos[1], "members") else {
        panic!()
    };
    for m in inner {
        let Ipld::List(m) = m else { panic!() };
        assert_eq!(m[2], Ipld::Null);
    }
    crate::check::document(&doc).unwrap();
}

/// M4: -0.0 is kept as is (DAG-CBOR allows it; it is a distinct float).
#[test]
fn negative_zero_is_kept() {
    let doc = world("struct F(v: f64)\nx { F: {-0.0} }\n")
        .document(&["x"])
        .unwrap();
    let v = field(
        field(field(field(entity(&doc, "x"), "components"), "F"), "value"),
        "v",
    );
    let Ipld::Float(f) = v else { panic!("{v:?}") };
    assert!(*f == 0.0 && f.is_sign_negative());
}

#[test]
fn roots_and_set() {
    let mut w = world(SITE);
    assert_eq!(
        w.roots().unwrap(),
        [
            "Author", "Card", "Flags", "Labels", "Page", "Status", "Title", "Vec2", "people",
            "site"
        ]
    );
    w.set(
        "site.zeta",
        "Title",
        &Ipld::Map([("text".to_string(), s("Zeta again"))].into()),
    )
    .unwrap();
    let doc = w.document(&["site"]).unwrap();
    let t = field(
        field(field(entity(&doc, "site.zeta"), "components"), "Title"),
        "value",
    );
    assert_eq!(field(t, "text"), &s("Zeta again"));
    // Checked against the layout: unknown members, wrong types and bad constants are refused.
    let bad = Ipld::Map([("txt".to_string(), s("x"))].into());
    assert!(w.set("site.zeta", "Title", &bad).is_err());
    assert!(w.set("site.zeta", "Status", &s("Nope")).is_err());
    w.set("site.zeta", "Status", &s("Done")).unwrap();
    let e = w.set("nowhere", "Title", &bad).unwrap_err();
    assert_eq!(e, "no entity named nowhere");
}

#[test]
fn bytes_and_blobs() {
    let mut w = world(
        "struct File(name: string, head: sync.Bytes, body: sync.Blob)\nf { File: {name: \"a\", head: [1, 2, 3], body: []} }\n",
    );
    // A pseudo-random 1 MiB body, so FastCDC cuts it into several chunks.
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let body: Vec<u8> = (0..1 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let file = |body: &[u8]| {
        Ipld::Map(
            [
                ("name".to_string(), s("a")),
                ("head".to_string(), Ipld::Bytes(vec![1, 2, 3])),
                ("body".to_string(), Ipld::Bytes(body.to_vec())),
            ]
            .into(),
        )
    };
    w.set("f", "File", &file(&body)).unwrap();
    let a = w.blocks(&["f"]).unwrap();
    let doc = w.document(&["f"]).unwrap();
    let v = field(
        field(field(entity(&doc, "f"), "components"), "File"),
        "value",
    );
    assert_eq!(field(v, "head"), &Ipld::Bytes(vec![1, 2, 3]));
    let Ipld::Link(link) = field(v, "body") else {
        panic!()
    };
    assert_eq!(link.codec(), crate::DAG_CBOR);
    let mut store = MemoryStore::default();
    a.store(&mut store);
    assert_eq!(crate::read_blob(&store, link).unwrap(), body);
    // One byte changed near the end: only the chunks around it are new.
    let mut next = body.clone();
    next[(1 << 20) - 100] ^= 1;
    w.set("f", "File", &file(&next)).unwrap();
    let b = w.blocks(&["f"]).unwrap();
    let raw = |bs: &crate::Blocks| -> std::collections::HashSet<cid::Cid> {
        bs.blocks
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| c.codec() == crate::RAW)
            .collect()
    };
    let (ra, rb) = (raw(&a), raw(&b));
    assert!(ra.len() >= 8, "{} chunks", ra.len());
    assert_eq!(rb.difference(&ra).count(), 1);
}

#[test]
fn sync_modes() {
    let w = world(
        "struct Cursor(x: f32)\nCursor { (Sync, Ephemeral) }\nstruct Score(v: i32)\nScore { (Sync, LatestOnly) }\ne { Cursor: {1}\n Score: {2} }\n",
    );
    let doc = w.document(&["e"]).unwrap();
    let comps = field(entity(&doc, "e"), "components");
    let Ipld::Map(cm) = comps else { panic!() };
    assert!(cm.contains_key("Score") && !cm.contains_key("Cursor"));
    let Ipld::Map(layouts) = field(&doc, "layouts") else {
        panic!()
    };
    assert!(!layouts.contains_key("Cursor"));
    assert_eq!(field(&layouts["Score"], "sync"), &s("LatestOnly"));
    let eph = w.ephemeral(&["e"]).unwrap();
    let Ipld::List(es) = field(&eph, "entities") else {
        panic!()
    };
    let c = field(field(field(&es[0], "components"), "Cursor"), "value");
    assert_eq!(field(c, "x"), &Ipld::Float(1.0));
}

#[test]
fn merge_states() {
    use crate::{Rule, merge_state};
    let stamp =
        |w: i128, a: i128| Ipld::List(vec![Ipld::Integer(w), Ipld::Integer(0), Ipld::Integer(a)]);
    // The merge crate holds a register's value as a u64 (the caller's handle for the field value).
    let reg = |w: i128, v: i128| {
        Ipld::Map(
            [
                ("stamp".to_string(), stamp(w, 1)),
                ("value".to_string(), Ipld::Integer(v)),
            ]
            .into(),
        )
    };
    assert_eq!(
        merge_state(Rule::LastWriter, &reg(1, 10), &reg(2, 20)).unwrap(),
        reg(2, 20)
    );
    assert_eq!(
        merge_state(Rule::LastWriter, &reg(2, 20), &reg(1, 10)).unwrap(),
        reg(2, 20)
    );
    let counter = |inc: &[(i128, i128)]| {
        let l = inc
            .iter()
            .map(|(a, n)| Ipld::List(vec![Ipld::Integer(*a), Ipld::Integer(*n)]))
            .collect();
        Ipld::Map(
            [
                ("inc".to_string(), Ipld::List(l)),
                ("dec".to_string(), Ipld::List(vec![])),
            ]
            .into(),
        )
    };
    let m = merge_state(Rule::Counter, &counter(&[(1, 3)]), &counter(&[(2, 4)])).unwrap();
    assert_eq!(m, counter(&[(1, 3), (2, 4)]));
    assert!(merge_state(Rule::LastWriter, &reg(1, 10), &counter(&[])).is_err());
}

/// The layout record of the type at `base` in a document.
fn record<'a>(doc: &'a Ipld, base: &str) -> &'a Ipld {
    let Ipld::Map(ls) = field(doc, "layouts") else {
        panic!("layouts is not a map")
    };
    ls.values()
        .find(|r| field(r, "base") == &s(base))
        .unwrap_or_else(|| panic!("no layout of {base}"))
}

fn member<'a>(rec: &'a Ipld, name: &str) -> &'a Ipld {
    let Ipld::List(ms) = field(field(rec, "layout"), "members") else {
        panic!("not a struct")
    };
    ms.iter()
        .find_map(|m| match m {
            Ipld::List(m) if m[0] == s(name) => Some(&m[1]),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no member {name}"))
}

/// Ratio's `den` has its own range; Gauge's `level` is a `Percent`, a logical type over u8.
const RULED: &str = "struct Ratio(num: u32, den: u32)\nPercent {\n  (IsA, flecs.meta.u8)\n  sync.Range: {max: 100}\n}\nstruct Gauge(level: u8)\nRatio {\n  den {\n    sync.Range: {min: 1}\n  }\n}\nGauge {\n  level {\n    (IsA, Percent)\n  }\n}\n";

#[test]
fn script_member_rules() {
    let mut w = world(&format!(
        "{RULED}r {{\n  Ratio: {{num: 1, den: 2}}\n  Gauge: {{level: 50}}\n}}\nbad {{\n  Ratio: {{num: 1, den: 0}}\n}}\n"
    ));
    let doc = w.document(&["r"]).unwrap();
    let rule = |k: &str, v: Ipld| Ipld::Map([(k.to_string(), v)].into());
    let logical = |name: Ipld, of: &str, rules: Ipld| {
        Ipld::Map(
            [
                ("logical".to_string(), name),
                ("of".to_string(), s(of)),
                ("rules".to_string(), rules),
            ]
            .into(),
        )
    };
    assert_eq!(
        member(record(&doc, "Ratio"), "den"),
        &logical(
            Ipld::Null,
            "u32",
            rule("range", Ipld::List(vec![Ipld::Integer(1), Ipld::Null]))
        )
    );
    assert_eq!(member(record(&doc, "Ratio"), "num"), &s("u32"));
    assert_eq!(
        member(record(&doc, "Gauge"), "level"),
        &logical(
            s("Percent"),
            "u8",
            rule("range", Ipld::List(vec![Ipld::Null, Ipld::Integer(100)]))
        )
    );

    // A value Flecs Script wrote past a rule is refused by the save, with the member's path; never clamped.
    let e = w.document(&["bad"]).unwrap_err();
    assert!(e.contains("member den: 0 is below the minimum 1"), "{e}");
    // A write through `set` is checked before it lands.
    let ratio = |den| {
        Ipld::Map(
            [
                ("num".to_string(), Ipld::Integer(1)),
                ("den".to_string(), Ipld::Integer(den)),
            ]
            .into(),
        )
    };
    let e = w.set("r", "Ratio", &ratio(0)).unwrap_err();
    assert!(e.contains("member den: 0 is below the minimum 1"), "{e}");
    let level = Ipld::Map([("level".to_string(), Ipld::Integer(101))].into());
    let e = w.set("r", "Gauge", &level).unwrap_err();
    assert!(e.contains("101 is above the maximum 100 (Percent)"), "{e}");
    w.set("r", "Ratio", &ratio(3)).unwrap();
}

/// The compatibility classifier's table (SPEC.md, "Compatibility"): each row an old and a new declaration of `T`.
#[test]
fn compat_table() {
    use crate::{Compat, compare};
    const BASE: &str = "Meters {}\nFeet {}\n";
    let rec = |decl: &str, value: &str| {
        let w = world(&format!("{BASE}{decl}\nt {{\n  T: {{{value}}}\n}}\n"));
        record(&w.document(&["t"]).unwrap(), "T").clone()
    };
    let with = |ty: &str, annot: &str| {
        format!("struct T(v: {ty})\nT {{\n  v {{\n{annot}\n  }}\n}}")
    };
    let n = |ty: &str, annot: &str| (with(ty, annot), "v: 5".to_string());
    let cat = |names: &str| {
        (
            format!(
                "Colors {{\n  sync.Catalog\n  {names}\n}}\n{}",
                with("string", "(sync.RefersTo, Colors)")
            ),
            "v: \"red\"".to_string(),
        )
    };
    /// A declaration of `T` and a value of it.
    type Decl = (String, String);
    let rows: Vec<(Decl, Decl, Compat)> = vec![
        // Nothing changed.
        (n("u32", ""), n("u32", ""), Compat::Safe),
        // Range: added or tightened needs a check, loosened or dropped is safe.
        (
            n("u32", ""),
            n("u32", "sync.Range: {min: 1}"),
            Compat::NeedsCheck,
        ),
        (
            n("u32", "sync.Range: {max: 50}"),
            n("u32", "sync.Range: {max: 10}"),
            Compat::NeedsCheck,
        ),
        (
            n("u32", "sync.Range: {max: 10}"),
            n("u32", "sync.Range: {max: 50}"),
            Compat::Safe,
        ),
        (n("u32", "sync.Range: {min: 1}"), n("u32", ""), Compat::Safe),
        // Unit: added is safe, changed needs a migration.
        (n("f32", ""), n("f32", "(sync.Unit, Meters)"), Compat::Safe),
        (
            n("f32", "(sync.Unit, Meters)"),
            n("f32", "(sync.Unit, Feet)"),
            Compat::NeedsMigration,
        ),
        // A physical change needs a migration.
        (n("u32", ""), n("u64", ""), Compat::NeedsMigration),
        (
            n("u32", ""),
            (
                "struct T(v: u32, w: u32)".into(),
                "v: 5, w: 1".into(),
            ),
            Compat::NeedsMigration,
        ),
        // Catalog: a name added is safe, one removed needs a migration, refers_to added needs a check.
        (
            cat("red {}\n  green {}"),
            cat("red {}\n  green {}\n  blue {}"),
            Compat::Safe,
        ),
        (
            cat("red {}\n  green {}"),
            cat("red {}"),
            Compat::NeedsMigration,
        ),
        (
            (with("string", ""), "v: \"red\"".into()),
            cat("red {}"),
            Compat::NeedsCheck,
        ),
    ];
    for (i, ((od, ov), (nd, nv), want)) in rows.iter().enumerate() {
        let v = compare(&rec(od, ov), &rec(nd, nv)).unwrap();
        assert_eq!(v.compat, *want, "row {i}: {:?}", v.changes);
    }

    // A migration registry takes Safe and NeedsCheck changes as by-name edges and refuses the rest, naming them.
    let mut m = crate::Migrations::new();
    let (u32_, u64_, ranged) = (n("u32", ""), n("u64", ""), n("u32", "sync.Range: {min: 1}"));
    let e = m
        .compatible(rec(&u32_.0, &u32_.1), rec(&u64_.0, &u64_.1))
        .unwrap_err();
    assert!(e.contains("v"), "{e}");
    let v = m
        .compatible(rec(&u32_.0, &u32_.1), rec(&ranged.0, &ranged.1))
        .unwrap();
    assert_eq!(v.compat, Compat::NeedsCheck);
    assert!(!m.is_empty());
}

/// `World::layout_record` is the record a document carries for the type, so a writer typing values by it and a
/// reader checking them by the document agree.
#[test]
fn layout_record_matches_the_document() {
    let w = world(SITE);
    let doc = w.document(&["site"]).unwrap();
    let want = field(field(&doc, "layouts"), "Card");
    assert_eq!(&w.layout_record("Card").unwrap(), want);
    assert!(w.layout_record("NoSuchType").is_err());
}
