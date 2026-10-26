//! A described type's layout, value and round trip; registering it in a world (Flecs struct, opaque, checked meta);
//! and a snapshot of it.

use std::collections::BTreeMap;
use std::ffi::CString;

use flecs_describe::{Describe, Flecs, Ipld, R, Refs, Resolve, component, register, with};
use flecs_snapshot::World;
use flecs_snapshot::check::{self, Blobs};

#[derive(Describe, Clone, Debug, Default, PartialEq)]
#[repr(C)]
struct Pos {
    x: f32,
    y: f32,
}

/// Pos's members in the other order.
#[derive(Describe, Clone, Debug, Default, PartialEq)]
#[repr(C)]
struct Swapped {
    y: f32,
    x: f32,
}

#[derive(Describe, Clone, Debug, Default, PartialEq)]
enum Shape {
    #[default]
    Dot,
    Circle {
        r: f32,
    },
    Segment(Pos, Pos),
}

#[derive(Describe, Clone, Copy, Debug, Default, PartialEq)]
#[repr(u8)]
enum Mode {
    #[default]
    Idle,
    Busy = 5,
}

#[derive(Describe, Clone, Debug, Default, PartialEq)]
struct Card {
    name: String,
    #[describe(merge = "AddWins")]
    weights: Vec<f32>,
    #[describe(merge = "Counter")]
    hits: u32,
    best: Option<u32>,
    shape: Shape,
    mode: Mode,
    at: Pos,
    pair: (u8, i64),
    scores: BTreeMap<String, u32>,
    #[describe(with = with::bytes)]
    thumb: Vec<u8>,
    #[describe(runtime)]
    cache: Vec<u64>,
}

struct NoRefs;

impl Refs for NoRefs {
    fn reference(&mut self, e: u64) -> R<Ipld> {
        Err(format!("no entity expected, got #{e}"))
    }
}

impl Resolve for NoRefs {
    fn entity(&mut self, r: &Ipld) -> R<u64> {
        Err(format!("no ref expected, got {r:?}"))
    }
}

fn card() -> Card {
    Card {
        name: "ace".into(),
        weights: vec![0.5, 2.0],
        hits: 3,
        best: Some(7),
        shape: Shape::Segment(Pos { x: 1.0, y: 2.0 }, Pos { x: 3.0, y: 4.0 }),
        mode: Mode::Busy,
        at: Pos { x: -1.0, y: 0.25 },
        pair: (9, -4),
        scores: [("b".to_string(), 2), ("a".to_string(), 1)]
            .into_iter()
            .collect(),
        thumb: vec![1, 2, 3],
        cache: vec![42],
    }
}

fn map(entries: &[(&str, Ipld)]) -> Ipld {
    Ipld::Map(
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

#[test]
fn round_trip() {
    let c = card();
    let l = Card::layout();
    check::described_layout(&l).unwrap();
    let v = c.read(&mut NoRefs).unwrap();
    check::value(&v, &l, Blobs::Bytes).unwrap();

    let Ipld::Map(m) = &v else {
        panic!("not a map")
    };
    assert_eq!(m["cache"], Ipld::Null);
    assert_eq!(m["thumb"], Ipld::Bytes(vec![1, 2, 3]));
    assert_eq!(m["mode"], Ipld::String("Busy".into()));
    assert_eq!(m["best"], Ipld::List(vec![Ipld::Integer(7)]));
    assert_eq!(
        m["scores"],
        Ipld::List(vec![
            Ipld::List(vec![Ipld::String("a".into()), Ipld::Integer(1)]),
            Ipld::List(vec![Ipld::String("b".into()), Ipld::Integer(2)]),
        ])
    );
    let Ipld::Map(seg) = &m["shape"] else {
        panic!("shape is not a union value")
    };
    assert!(seg.contains_key("Segment"));

    let back = Card::from_value(&v, &mut NoRefs).unwrap();
    // The runtime part is not saved: it comes back as its default.
    assert_eq!(back, Card { cache: vec![], ..c });

    // Rule slots carry the declared rules.
    let Ipld::Map(lm) = &l else { panic!() };
    let Ipld::List(members) = &lm["members"] else {
        panic!()
    };
    let rule = |n: &str| {
        members
            .iter()
            .find_map(|x| match x {
                Ipld::List(x) if x[0] == Ipld::String(n.into()) => Some(x[2].clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(rule("weights"), Ipld::String("AddWins".into()));
    assert_eq!(rule("hits"), Ipld::String("Counter".into()));
    assert_eq!(rule("name"), Ipld::Null);
}

#[test]
fn bad_values_are_errors() {
    let mut v = card().read(&mut NoRefs).unwrap();
    let Ipld::Map(m) = &mut v else { panic!() };
    m.insert("hits".into(), Ipld::Integer(-1));
    let e = Card::from_value(&v, &mut NoRefs).unwrap_err();
    assert!(e.starts_with("member hits"), "{e}");

    assert!(f32::from_value(&Ipld::Float(0.1), &mut NoRefs).is_err());
    assert!(Mode::from_value(&Ipld::String("Gone".into()), &mut NoRefs).is_err());
    let dup = Ipld::List(vec![
        Ipld::List(vec![Ipld::String("a".into()), Ipld::Integer(1)]),
        Ipld::List(vec![Ipld::String("a".into()), Ipld::Integer(2)]),
    ]);
    assert!(BTreeMap::<String, u32>::from_value(&dup, &mut NoRefs).is_err());
}

#[test]
fn unit_enum_layout() {
    let l = Mode::layout();
    assert_eq!(
        l,
        map(&[
            ("enum", Ipld::String("describe::Mode".into())),
            ("underlying", Ipld::String("u8".into())),
            (
                "constants",
                Ipld::List(vec![
                    Ipld::List(vec![Ipld::String("Idle".into()), Ipld::Integer(0)]),
                    Ipld::List(vec![Ipld::String("Busy".into()), Ipld::Integer(5)]),
                ])
            ),
        ])
    );
    assert!(
        matches!(Mode::flecs(), Flecs::Enum { underlying, .. } if underlying == flecs_c::EcsU8)
    );
    assert!(Pos::flecs().expressible());
    assert!(!Card::flecs().expressible());
}

fn lookup(w: &mut World, path: &str) -> u64 {
    let c = CString::new(path).unwrap();
    // SAFETY: plain lookup in a live world.
    unsafe { flecs_c::ecs_lookup(w.as_ptr(), c.as_ptr()) }
}

#[test]
fn register_components() {
    let mut w = World::new().unwrap();
    // SAFETY: the world is live and used only here; the components hold values of their Rust types.
    unsafe {
        let pos = component::<Pos>(w.as_ptr(), "game.Pos").unwrap();
        let card = component::<Card>(w.as_ptr(), "game.Card").unwrap();
        // Registering again checks the Flecs meta made the first time.
        register::<Pos>(w.as_ptr(), pos).unwrap();
        // Pos's Flecs struct has Pos's members, so Swapped's description does not fit it.
        assert!(register::<Swapped>(w.as_ptr(), pos).is_err());

        let d = flecs_snapshot::described(w.as_ptr(), card).expect("described");
        let Ipld::Map(m) = &d.layout else { panic!() };
        assert_eq!(m["struct"], Ipld::String("game.Card".into()));
    }
}

#[test]
fn script_struct_is_checked() {
    let mut w = World::new().unwrap();
    w.run("types", "struct Pos {\n  x = f32\n  y = f32\n}\n")
        .unwrap();
    let e = lookup(&mut w, "Pos");
    assert_ne!(e, 0);
    // SAFETY: as above; Pos's values are Pos (a #[repr(C)] pair of f32, the script struct's layout).
    unsafe {
        register::<Pos>(w.as_ptr(), e).unwrap();
        let err = register::<Swapped>(w.as_ptr(), e).unwrap_err();
        assert!(err.contains("member"), "{err}");
    }
}

/// The component's value in a document: the first `components` map holding it.
fn find<'a>(v: &'a Ipld, comp: &str) -> Option<&'a Ipld> {
    match v {
        Ipld::Map(m) => {
            if let Some(Ipld::Map(cs)) = m.get("components")
                && let Some(Ipld::Map(c)) = cs.get(comp)
            {
                return c.get("value");
            }
            m.values().find_map(|x| find(x, comp))
        }
        Ipld::List(l) => l.iter().find_map(|x| find(x, comp)),
        _ => None,
    }
}

#[test]
fn snapshot_uses_the_description() {
    let mut w = World::new().unwrap();
    // SAFETY: as above.
    unsafe { component::<Card>(w.as_ptr(), "Card") }.unwrap();
    w.run("scene", "e {}\n").unwrap();
    let v = card().read(&mut NoRefs).unwrap();
    w.set("e", "Card", &v).unwrap();
    let doc = w.document(&["e"]).unwrap();
    assert_eq!(find(&doc, "Card"), Some(&v));
}

/// rapier's pose: `glamx.Pose3 {rotation, translation}`; its padding is not saved, and its Flecs struct is the full
/// 32 bytes with the two members at their measured offsets.
#[cfg(feature = "glamx")]
#[test]
fn glamx_pose3() {
    let p = glamx::Pose3 {
        rotation: glam::Quat::from_rotation_y(0.5),
        translation: glam::Vec3::new(1.0, -2.0, 3.5),
        padding: 7,
    };
    let l = glamx::Pose3::layout();
    check::described_layout(&l).unwrap();
    let Ipld::Map(lm) = &l else { panic!() };
    assert_eq!(lm["struct"], Ipld::String("glamx.Pose3".into()));
    let Ipld::List(members) = &lm["members"] else {
        panic!()
    };
    let names: Vec<&Ipld> = members
        .iter()
        .map(|m| match m {
            Ipld::List(m) => &m[0],
            _ => panic!(),
        })
        .collect();
    assert_eq!(
        names,
        [
            &Ipld::String("rotation".into()),
            &Ipld::String("translation".into())
        ]
    );

    let v = p.read(&mut NoRefs).unwrap();
    check::value(&v, &l, Blobs::Bytes).unwrap();
    let Ipld::Map(m) = &v else { panic!() };
    assert!(!m.contains_key("padding"));
    let back = glamx::Pose3::from_value(&v, &mut NoRefs).unwrap();
    assert_eq!(
        (back.rotation, back.translation, back.padding),
        (p.rotation, p.translation, 0)
    );

    let Flecs::Struct(s) = glamx::Pose3::flecs() else {
        panic!("not a struct")
    };
    assert_eq!(s.size, core::mem::size_of::<glamx::Pose3>());
    let offsets: Vec<(String, usize)> = s
        .members
        .iter()
        .map(|m| (m.name.clone(), m.offset))
        .collect();
    assert_eq!(
        offsets,
        [("rotation".to_string(), 0), ("translation".to_string(), 16)]
    );

    let mut w = World::new().unwrap();
    // SAFETY: the world is live and used only here; the component holds Pose3 values.
    unsafe {
        let c = component::<glamx::Pose3>(w.as_ptr(), "Pose").unwrap();
        register::<glamx::Pose3>(w.as_ptr(), c).unwrap();
    }
    w.run("scene", "e {}\n").unwrap();
    w.set("e", "Pose", &v).unwrap();
    let doc = w.document(&["e"]).unwrap();
    assert_eq!(find(&doc, "Pose"), Some(&v));
}

/// Two handle types of one size and alignment under two Rust paths, neither described.
#[allow(dead_code)]
mod one {
    pub struct Handle(pub std::sync::Arc<u32>);
}
#[allow(dead_code)]
mod two {
    pub struct Handle(pub String);
}

#[allow(dead_code)]
#[derive(Describe)]
#[describe(name = "Holder")]
struct HolderOne {
    h: one::Handle,
    #[describe(runtime)]
    scratch: Vec<u8>,
}

#[allow(dead_code)]
#[derive(Describe)]
#[describe(name = "Holder")]
struct HolderTwo {
    h: std::sync::Arc<two::Handle>,
    #[describe(runtime)]
    scratch: Vec<String>,
}

#[allow(dead_code)]
#[derive(Describe, Default)]
#[describe(runtime)]
struct Cache(Vec<u64>);

/// A runtime part (implicit or marked) is recorded by its size and alignment, never its Rust path: a compiler that
/// moves `Arc` or a move to another module leaves its layout, and so its hash, as it was.
#[test]
fn runtime_layout_has_no_rust_path() {
    assert_eq!(HolderOne::layout(), HolderTwo::layout());
    check::described_layout(&HolderOne::layout()).unwrap();
    let body = |size: usize, align: usize, implicit: bool| {
        map(&[(
            "runtime",
            map(&[
                ("size", Ipld::Integer(size as i128)),
                ("align", Ipld::Integer(align as i128)),
                ("implicit", Ipld::Bool(implicit)),
            ]),
        )])
    };
    let Ipld::Map(lm) = HolderOne::layout() else {
        panic!()
    };
    let Ipld::List(members) = &lm["members"] else {
        panic!()
    };
    let ptr = core::mem::size_of::<usize>();
    let vec = core::mem::size_of::<Vec<u8>>();
    assert_eq!(
        members
            .iter()
            .map(|m| match m {
                Ipld::List(m) => m[1].clone(),
                _ => panic!(),
            })
            .collect::<Vec<_>>(),
        [body(ptr, ptr, true), body(vec, ptr, false)]
    );
    assert_eq!(Cache::layout(), body(vec, ptr, false));
    let Ipld::Map(b) = body(ptr, ptr, true) else {
        panic!()
    };
    assert!(flecs_describe::runtime_is_implicit(&b["runtime"]));
}
