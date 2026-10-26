//! Logical refinements of described types (SPEC.md, "Logical types"): NonZero, a field's range and unit, a type's
//! `validate` check and a catalog reference, each round-tripped and refused with the member's path.

use std::num::NonZero;

use flecs_describe::{Catalog, Describe, Ipld, R, Refs, Resolve, component};
use flecs_snapshot::World;
use flecs_snapshot::check::{self, Blobs};

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

fn map(entries: &[(&str, Ipld)]) -> Ipld {
    Ipld::Map(
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

#[derive(Debug, PartialEq)]
struct Material {
    name: &'static str,
}

static STEEL: Material = Material { name: "steel" };
static GLASS: Material = Material { name: "glass" };
static MATERIALS: [&Material; 2] = [&STEEL, &GLASS];

impl Catalog for Material {
    const CATALOG: &'static str = "test.materials";
    fn entries() -> &'static [&'static Material] {
        &MATERIALS
    }
    fn name(&self) -> &str {
        self.name
    }
}

fn even(t: &Tank) -> Result<(), String> {
    if t.cells.is_multiple_of(2) {
        Ok(())
    } else {
        Err(format!("{} cells is odd", t.cells))
    }
}

#[derive(Describe, Clone, Debug, PartialEq)]
#[describe(name = "test.Tank", validate = even)]
struct Tank {
    #[describe(range(min = 0.0, max = 1.0), unit = "test.units.Fraction", finite)]
    fill: f32,
    #[describe(range(min = -40, max = 60))]
    celsius: i16,
    den: NonZero<u32>,
    cells: u32,
    wall: &'static Material,
}

impl Default for Tank {
    fn default() -> Tank {
        Tank {
            fill: 0.5,
            celsius: 20,
            den: NonZero::new(1).unwrap(),
            cells: 2,
            wall: &STEEL,
        }
    }
}

#[test]
fn nonzero_is_a_bare_integer() {
    let l = <NonZero<u32>>::layout();
    assert_eq!(
        l,
        map(&[
            ("logical", Ipld::String("NonZero<u32>".into())),
            ("of", Ipld::String("u32".into())),
            (
                "rules",
                map(&[("range", Ipld::List(vec![Ipld::Integer(1), Ipld::Null]))])
            ),
        ])
    );
    let v = NonZero::new(7u32).unwrap().read(&mut NoRefs).unwrap();
    assert_eq!(v, Ipld::Integer(7));
    let e = check::value(&Ipld::Integer(0), &l, Blobs::Bytes).unwrap_err();
    assert!(e.contains("0 is below the minimum 1 (NonZero<u32>)"), "{e}");
    assert!(<NonZero<u32>>::from_value(&Ipld::Integer(0), &mut NoRefs).is_err());
    // A signed NonZero cannot say "not zero" with a range: it names the check instead.
    let Ipld::Map(m) = <NonZero<i8>>::layout() else {
        panic!()
    };
    assert_eq!(
        m["rules"],
        map(&[(
            "validated",
            Ipld::List(vec![Ipld::String("sync.checks.NonZero".into())])
        )])
    );
}

#[test]
fn refined_members_round_trip_and_refuse() {
    let mut w = World::new().unwrap();
    // SAFETY: the world is live and used only here; the component holds Tank values.
    unsafe { component::<Tank>(w.as_ptr(), "test.Tank") }.unwrap();
    w.run("scene", "t {}\n").unwrap();

    let good = Tank::default().read(&mut NoRefs).unwrap();
    let Ipld::Map(m) = &good else { panic!() };
    assert_eq!(m["wall"], Ipld::String("steel".into()));
    assert_eq!(m["den"], Ipld::Integer(1));
    w.set("t", "test.Tank", &good).unwrap();
    w.document(&["t"]).unwrap();
    assert_eq!(
        Tank::from_value(&good, &mut NoRefs).unwrap(),
        Tank::default()
    );

    let with = |k: &str, v: Ipld| {
        let mut m = m.clone();
        m.insert(k.into(), v);
        Ipld::Map(m)
    };
    for (k, v, msg) in [
        (
            "fill",
            Ipld::Float(1.5),
            "member fill: 1.5 is above the maximum 1",
        ),
        (
            "celsius",
            Ipld::Integer(-41),
            "member celsius: -41 is below the minimum -40",
        ),
        (
            "den",
            Ipld::Integer(0),
            "member den: 0 is below the minimum 1 (NonZero<u32>)",
        ),
        (
            "wall",
            Ipld::String("wood".into()),
            "member wall: \"wood\" is not a name in the catalog test.materials",
        ),
        ("cells", Ipld::Integer(3), "3 cells is odd"),
    ] {
        let e = w.set("t", "test.Tank", &with(k, v)).unwrap_err();
        assert!(e.contains(msg), "{k}: {e}");
    }
    // The value is unchanged by the refused writes.
    w.document(&["t"]).unwrap();

    // The member entities carry the rules for Flecs (a range the inspector shows) and the world (a catalog).
    let mut lookup = |p: &str| {
        let c = std::ffi::CString::new(p).unwrap();
        // SAFETY: plain lookup in a live world.
        unsafe { flecs_c::ecs_lookup(w.as_ptr(), c.as_ptr()) }
    };
    assert_ne!(lookup("test.materials.glass"), 0);
    assert_ne!(lookup("sync.checks.test\\.Tank"), 0);
}
