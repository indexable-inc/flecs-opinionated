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
