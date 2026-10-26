//! Host mode against the space game's Flecs (flecs_ecs at the game's rev): one Flecs in the process, the game's
//! `Uid {hi, lo}` and `Persist`, components registered by flecs_ecs's own meta.

use std::time::Instant;

use flecs_ecs::prelude::*;
use flecs_snapshot::{Encode, Host, Migrations, Restore, Selection};

#[derive(Component, Default, Clone, Copy)]
#[flecs(meta)]
pub struct Uid {
    pub hi: u64,
    pub lo: u64,
}

#[derive(Component, Default, Clone, Copy)]
pub struct Persist;

#[derive(Component, Default, Clone, Copy)]
#[flecs(meta)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Component, Default, Clone, Copy)]
#[flecs(meta)]
pub struct PosV2 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

#[derive(Component, Default, Clone, Copy)]
#[flecs(meta)]
pub struct Vel {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Component, Default, Clone, Copy)]
#[flecs(meta)]
pub struct Mass {
    pub kg: f64,
}

#[derive(Component, Default, Clone, Copy)]
#[flecs(meta)]
pub struct Health {
    pub hp: f32,
    pub max: f32,
}

#[derive(Component, Default, Clone)]
#[flecs(meta)]
pub struct Label {
    pub text: String,
}

#[derive(Component, Clone, Copy)]
#[flecs(meta)]
pub struct Link {
    pub to: Entity,
}

impl Default for Link {
    fn default() -> Link {
        Link { to: Entity::null() }
    }
}

#[derive(Component, Default, Clone, Copy)]
pub struct Powered;

#[derive(Component, Default, Clone, Copy)]
pub struct Sealed;

fn register(w: &World) {
    w.component::<Uid>();
    w.component::<Persist>();
    w.component::<Pos>();
    w.component::<PosV2>();
    w.component::<Vel>();
    w.component::<Mass>();
    w.component::<Health>();
    w.component::<Label>();
    w.component::<Link>();
    w.component::<Powered>();
    w.component::<Sealed>();
}

fn host(w: &World) -> Host {
    // SAFETY: the World outlives the Host in every test, and the tests are single-threaded per world.
    unsafe { Host::new(w.ptr_mut().cast(), "Uid", "Persist") }.unwrap()
}

/// `n` saved entities in 12 archetypes under `n / 64` named parents, the game's save at about its size.
fn scene(w: &World, n: u64) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut parents = Vec::new();
    let mut next = 1u64;
    for p in 0..n.div_ceil(64) {
        let e = w
            .entity_named(&format!("ship{p}"))
            .set(Uid { hi: 7, lo: next })
            .add(Persist::id())
            .set(Mass { kg: p as f64 });
        next += 1;
        parents.push(e.id());
        out.push(e.id());
    }
    let mut i = 0u64;
    while (out.len() as u64) < n {
        let parent = parents[(i % parents.len() as u64) as usize];
        let e = w
            .entity()
            .child_of(parent)
            .set(Uid { hi: 7, lo: next })
            .add(Persist::id());
        next += 1;
        let f = i as f32;
        e.set(Pos {
            x: f,
            y: f * 0.5,
            z: -f,
        });
        let k = i % 12;
        if k & 1 != 0 {
            e.set(Vel {
                x: 1.0,
                y: 0.0,
                z: f,
            });
        }
        if k & 2 != 0 {
            e.set(Health { hp: f, max: 100.0 });
        }
        if k & 4 != 0 {
            e.set(Label {
                text: format!("part {i}"),
            });
        }
        if k == 3 || k == 7 || k == 11 {
            e.set(Link { to: parent });
        }
        if k >= 8 {
            e.add(Powered::id());
        }
        if k == 5 || k == 9 {
            e.add(Sealed::id());
        }
        out.push(e.id());
        i += 1;
    }
    out
}

#[test]
fn round_trip_through_flecs_ecs() {
    let a = World::new();
    register(&a);
    scene(&a, 300);
    let ha = host(&a);
    let doc = ha.encode(&Selection::Tagged, &Encode::default()).unwrap();
    let tree = ha.document(&Selection::Tagged, &Encode::default()).unwrap();
    assert_eq!(
        doc,
        serde_ipld_dagcbor::to_vec(&tree).unwrap(),
        "the direct writer matches the Ipld path"
    );
    let b = World::new();
    register(&b);
    let mut hb = host(&b);
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
    assert_eq!(r.entities.len(), 300);
    assert_eq!(
        hb.encode(&Selection::Tagged, &Encode::default()).unwrap(),
        doc
    );
}

#[test]
fn live_migration_through_flecs_ecs() {
    let a = World::new();
    register(&a);
    let ids = scene(&a, 200);
    let mut h = host(&a);
    let mut m = Migrations::new();
    m.by_name(
        h.layout_record("Pos").unwrap(),
        h.layout_record("PosV2").unwrap(),
        &[],
    )
    .unwrap();
    h.migrate_live(&m, "Pos", "PosV2").unwrap();
    let e = a.entity_from_id(ids[10]);
    assert!(!e.has(Pos::id()));
    e.get::<&PosV2>(|p| assert_eq!((p.x, p.w), (6.0, 0.0)));
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// The game's save size (about 6,805 entities in 12 archetypes). Run in release:
/// `cargo test --release --test host -- --ignored --nocapture bench`.
#[test]
#[ignore]
fn bench_game_size() {
    let n = 6805;
    let a = World::new();
    register(&a);
    let ids = scene(&a, n);
    let mut h = host(&a);
    let (sel, opts) = (Selection::Tagged, Encode::default());
    let t = Instant::now();
    let doc = h.encode(&sel, &opts).unwrap();
    let t_doc = ms(t);
    let zst = zstd::encode_all(&doc[..], 3).unwrap();
    let t = Instant::now();
    let full = h.blocks_full(&sel, &opts).unwrap();
    let t_full = ms(t);
    let t = Instant::now();
    let same = h.blocks(&sel, &opts).unwrap();
    let t_same = ms(t);
    assert_eq!(same.root, full.root);
    // 1% of the entities move.
    for (k, e) in ids.iter().enumerate().step_by(100) {
        a.entity_from_id(*e).set(Pos {
            x: k as f32,
            y: 1.0,
            z: 2.0,
        });
    }
    let t = Instant::now();
    let inc = h.blocks(&sel, &opts).unwrap();
    let t_inc = ms(t);
    assert_eq!(h.blocks_full(&sel, &opts).unwrap().root, inc.root);
    for (k, e) in ids.iter().enumerate().step_by(100) {
        a.entity_from_id(*e).set(Pos {
            x: k as f32,
            y: 3.0,
            z: 2.0,
        });
    }
    let t = Instant::now();
    let new = h.blocks_new(&sel, &opts).unwrap();
    let t_new = ms(t);
    assert_eq!(h.blocks_full(&sel, &opts).unwrap().root, new.root);
    let b = World::new();
    register(&b);
    let mut hb = host(&b);
    let t = Instant::now();
    hb.restore(&doc, &Migrations::new(), &mut Restore::default())
        .unwrap();
    let t_restore = ms(t);
    let mut m = Migrations::new();
    m.by_name(
        h.layout_record("Pos").unwrap(),
        h.layout_record("PosV2").unwrap(),
        &[],
    )
    .unwrap();
    let t = Instant::now();
    h.migrate_live(&m, "Pos", "PosV2").unwrap();
    let t_mig = ms(t);
    eprintln!(
        "BENCH entities={n} doc_bytes={} zstd3_bytes={} blocks={} encode_ms={t_doc:.2} blocks_full_ms={t_full:.2} \
         blocks_unchanged_ms={t_same:.2} blocks_1pct_ms={t_inc:.2} blocks_new_1pct_ms={t_new:.2} new_blocks={} restore_ms={t_restore:.2} migrate_live_ms={t_mig:.2}",
        doc.len(),
        zst.len(),
        full.blocks.len(),
        new.blocks.len(),
    );
}
