use flecs_c as sys;
use ipld_core::ipld::Ipld;

use super::*;
use crate::{MemoryStore, World};

const TYPES: &str = r#"
using flecs.meta
struct Uid(hi: u64, lo: u64)
Persist {}
Likes {}
struct Pos(x: f32, y: f32)
struct Link(to: entity)
prefab Ship {
  Pos: {1, 2}
}
"#;

const SCENE: &str = r#"
station {
  Uid: {0, 1}
  Persist
  Pos: {3, 4}
  dock {
    Uid: {0, 2}
    Persist
    Link: {to: station}
  }
}
probe : Ship {
  Uid: {0, 3}
  Persist
  (Likes, station)
}
"#;

/// A world with the types (and, optionally, more script); `script` gives no `sync.Id`, as in a host world.
fn world(extra: &[&str]) -> World {
    let mut w = World::new().unwrap();
    crate::world::script(w.as_ptr(), "types", TYPES).unwrap();
    for (i, code) in extra.iter().enumerate() {
        crate::world::script(w.as_ptr(), &format!("s{i}"), code).unwrap();
    }
    w
}

fn host(w: &mut World) -> Host {
    // SAFETY: the World outlives the Host in every test.
    unsafe { Host::new(w.as_ptr(), "Uid", "Persist") }.unwrap()
}

fn saved(w: &mut World) -> usize {
    let p = w.as_ptr();
    let tag = lookup(p, "Persist").unwrap();
    grow(|buf, cap| unsafe { sys::glue_with(p, tag, buf, cap) }).len()
}

fn ent(w: &mut World, path: &str) -> ecs_entity_t {
    lookup(w.as_ptr(), path).unwrap()
}

fn set_pos(w: &mut World, path: &str, x: f32, y: f32) {
    let p = w.as_ptr();
    let (e, pos) = (ent(w, path), lookup(p, "Pos").unwrap());
    let v = [x, y];
    // SAFETY: Pos is two f32s.
    unsafe { sys::ecs_set_id(p, e, pos, 8, v.as_ptr().cast()) };
}

#[test]
fn round_trip_into_a_fresh_world() {
    let mut a = world(&[SCENE]);
    let ha = host(&mut a);
    let doc = ha.encode(&Selection::Tagged, &Encode::default()).unwrap();
    let tree = ha.document(&Selection::Tagged, &Encode::default()).unwrap();
    assert_eq!(
        doc,
        crate::walk::encode(&tree),
        "the direct writer matches the Ipld path"
    );
    let mut b = world(&[]);
    let mut hb = host(&mut b);
    let r = hb
        .restore(
            &doc,
            &Migrations::new(),
            &mut Restore {
                verify: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r.entities.len(), 3);
    assert_eq!(r.report, Report::default());
    let again = hb.encode(&Selection::Tagged, &Encode::default()).unwrap();
    assert_eq!(doc, again, "a restored world saves to the same bytes");
    // Names and parents came back.
    let dock = ent(&mut b, "station.dock");
    assert_ne!(dock, 0);
    // A second restore with the same ids is refused and changes nothing.
    let before = saved(&mut b);
    let e = hb
        .restore(&doc, &Migrations::new(), &mut Restore::default())
        .unwrap_err();
    assert!(e.contains("already holds this id"), "{e}");
    assert_eq!(saved(&mut b), before);
}

#[test]
fn remint_loads_beside_a_held_id() {
    let mut a = world(&[SCENE]);
    let doc = host(&mut a)
        .encode(&Selection::Tagged, &Encode::default())
        .unwrap();
    // The target world already has an entity holding station's id.
    let mut b = world(&["other {\n  Uid: {0, 1}\n  Persist\n}\n"]);
    let mut hb = host(&mut b);
    let e = hb
        .restore(&doc, &Migrations::new(), &mut Restore::default())
        .unwrap_err();
    assert!(e.contains("already holds this id"), "{e}");
    assert_eq!(saved(&mut b), 1, "the refused restore left nothing behind");
    let r = hb
        .restore(
            &doc,
            &Migrations::new(),
            &mut Restore {
                ids: Ids::Remint(Box::new(|mut id| {
                    id[0] = 0xaa;
                    id
                })),
                verify: true,
            },
        )
        .unwrap();
    assert!(r.entities.iter().all(|(id, _)| id[0] == 0xaa));
    // dock's Link follows the new station, not the entity that held the old id.
    let p = b.as_ptr();
    let (dock, station, link) = (
        ent(&mut b, "station.dock"),
        ent(&mut b, "station"),
        lookup(p, "Link").unwrap(),
    );
    // SAFETY: Link is one entity.
    let to = unsafe { *sys::ecs_get_id(p, dock, link).cast::<ecs_entity_t>() };
    assert_eq!(to, station);
}

#[test]
fn incremental_blocks_match_full() {
    let mut a = world(&[SCENE]);
    let mut h = host(&mut a);
    let sel = Selection::Tagged;
    let opts = Encode::default();
    let b0 = h.blocks(&sel, &opts).unwrap();
    assert_eq!(
        h.blocks(&sel, &opts).unwrap().root,
        b0.root,
        "nothing changed"
    );
    set_pos(&mut a, "station", 9.0, 9.0);
    let b1 = h.blocks(&sel, &opts).unwrap();
    assert_ne!(b1.root, b0.root);
    assert_eq!(h.blocks_full(&sel, &opts).unwrap().root, b1.root);
    let mut store = MemoryStore::default();
    b0.store(&mut store);
    b1.store(&mut store);
    let d = diff(&store, &b0.root, &b1.root).unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].0[15], 1);
    assert_eq!(d[0].1, Kind::Changed);
}

#[test]
fn subtree_selection() {
    let mut a = world(&[SCENE]);
    let h = host(&mut a);
    let station = ent(&mut a, "station");
    let doc = h.document(&Selection::Subtree(station), &Encode::default());
    // dock refers to station (saved); probe is outside the subtree and left out.
    let Ipld::Map(m) = doc.unwrap() else { panic!() };
    assert_eq!(list(&m["entities"]).len(), 2);
}

#[test]
fn restore_migrates_and_refuses_unknown() {
    let mut a = world(&[SCENE]);
    let ha = host(&mut a);
    let doc = ha.encode(&Selection::Tagged, &Encode::default()).unwrap();
    let old = ha.layout_record("Pos").unwrap();
    // A world where Pos became PosV2 (f64 members and a new z).
    let types2 = TYPES
        .replace(
            "struct Pos(x: f32, y: f32)",
            "struct PosV2(z: f64, x: f64, y: f64)",
        )
        .replace("Pos: {1, 2}", "PosV2: {1, 2, 7}");
    let mut b = World::new().unwrap();
    crate::world::script(b.as_ptr(), "types", &types2).unwrap();
    let mut hb = host(&mut b);
    let new = hb.layout_record("PosV2").unwrap();
    // Without a migration: refused, nothing made.
    let e = hb
        .restore(&doc, &Migrations::new(), &mut Restore::default())
        .unwrap_err();
    assert!(e.contains("layout Pos"), "{e}");
    assert_eq!(saved(&mut b), 0);
    let mut m = Migrations::new();
    m.by_name(old, new, &[]).unwrap();
    let r = hb
        .restore(
            &doc,
            &m,
            &mut Restore {
                verify: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r.report.migrated, ["Pos -> PosV2"]);
    let station = ent(&mut b, "station");
    let pv2 = lookup(b.as_ptr(), "PosV2").unwrap();
    // SAFETY: PosV2 is z, x, y as f64.
    let v = unsafe { *sys::ecs_get_id(b.as_ptr(), station, pv2).cast::<[f64; 3]>() };
    assert_eq!(
        v,
        [0.0, 3.0, 4.0],
        "z from the constructor (zero), x and y widened"
    );
}

#[test]
fn live_migration_in_place() {
    let v2 = r#"
struct PosV2(x: f64, y: f64, z: f64)
"#;
    let mut a = world(&[SCENE, v2]);
    let mut h = host(&mut a);
    let mut m = Migrations::new();
    m.by_name(
        h.layout_record("Pos").unwrap(),
        h.layout_record("PosV2").unwrap(),
        &[],
    )
    .unwrap();
    let r = h.migrate_live(&m, "Pos", "PosV2").unwrap();
    assert_eq!(r.migrated, ["Pos -> PosV2"]);
    let p = a.as_ptr();
    let (pos, pv2) = (lookup(p, "Pos").unwrap(), lookup(p, "PosV2").unwrap());
    for path in ["station", "probe", "Ship"] {
        let e = ent(&mut a, path);
        // SAFETY: plain queries; PosV2 is three f64s.
        unsafe {
            assert!(!sys::ecs_owns_id(p, e, pos), "{path} lost Pos");
            let v = *sys::ecs_get_id(p, e, pv2).cast::<[f64; 3]>();
            assert_eq!(v[2], 0.0);
        }
    }
    // Running it again finds nothing to move.
    assert_eq!(
        h.migrate_live(&m, "Pos", "PosV2").unwrap().migrated.len(),
        0
    );
}

#[test]
fn a_bad_value_is_refused_before_the_world_changes() {
    let mut a = world(&[SCENE]);
    let doc = host(&mut a)
        .encode(&Selection::Tagged, &Encode::default())
        .unwrap();
    let Ipld::Map(mut d) = serde_ipld_dagcbor::from_slice::<Ipld>(&doc).unwrap() else {
        panic!()
    };
    let Some(Ipld::List(es)) = d.get_mut("entities") else {
        panic!()
    };
    let Ipld::Map(station) = &mut es[0] else {
        panic!()
    };
    let Some(Ipld::Map(cs)) = station.get_mut("components") else {
        panic!()
    };
    let Some(Ipld::Map(pos)) = cs.get_mut("Pos") else {
        panic!()
    };
    let Some(Ipld::Map(v)) = pos.get_mut("value") else {
        panic!()
    };
    v.insert("x".into(), Ipld::String("three".into()));
    let bad = serde_ipld_dagcbor::to_vec(&Ipld::Map(d)).unwrap();
    let mut b = world(&[]);
    let mut hb = host(&mut b);
    let e = hb
        .restore(&bad, &Migrations::new(), &mut Restore::default())
        .unwrap_err();
    assert!(e.contains("Pos") && e.contains("x"), "{e}");
    assert_eq!(saved(&mut b), 0);
}
