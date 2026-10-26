//! Described components in a flecs_ecs world: game-shaped Rust types (tuple structs, glam, Option, data enums,
//! vectors of tuples, entity refs, a runtime-only field) registered with flecs_ecs and described with
//! `#[derive(Describe)]`, saved and restored through host mode.

use flecs_describe::Describe;
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

#[derive(Component, Describe, Default, Clone, Debug, PartialEq)]
pub struct Heat(pub f32);

#[derive(Component, Describe, Default, Clone, Debug, PartialEq)]
pub struct Pose {
    pub at: glam::Vec3,
    pub turn: glam::Quat,
}

#[derive(Describe, Default, Clone, Debug, PartialEq)]
pub enum Door {
    #[default]
    Shut,
    Moving {
        to: f32,
        speed: f32,
    },
    Stuck(String),
}

#[derive(Component, Describe, Default, Clone, Debug, PartialEq)]
pub struct Hold {
    pub items: Vec<(u32, f32)>,
    pub owner: Option<Entity>,
    pub door: Door,
    #[describe(runtime)]
    pub scratch: Vec<u64>,
}

fn register(w: &World) {
    w.component::<Uid>();
    w.component::<Persist>();
    let heat = *w.component::<Heat>().id();
    let pose = *w.component::<Pose>().id();
    let hold = *w.component::<Hold>().id();
    let p = w.ptr_mut().cast();
    // SAFETY: each component holds values of its Rust type, made and dropped by flecs_ecs's hooks.
    unsafe {
        flecs_describe::register::<Heat>(p, heat).unwrap();
        flecs_describe::register::<Pose>(p, pose).unwrap();
        flecs_describe::register::<Hold>(p, hold).unwrap();
    }
}

fn host(w: &World) -> Host {
    // SAFETY: the World outlives the Host.
    unsafe { Host::new(w.ptr_mut().cast(), "Uid", "Persist") }.unwrap()
}

#[test]
fn described_components_round_trip() {
    let a = World::new();
    register(&a);
    let ship = a
        .entity_named("ship")
        .set(Uid { hi: 1, lo: 1 })
        .add(Persist::id())
        .set(Heat(3.5));
    let crate_ = a
        .entity()
        .child_of(ship)
        .set(Uid { hi: 1, lo: 2 })
        .add(Persist::id())
        .set(Pose {
            at: glam::Vec3::new(1.0, 2.0, 3.0),
            turn: glam::Quat::from_rotation_y(0.5),
        })
        .set(Hold {
            items: vec![(7, 0.25), (9, 1.5)],
            owner: Some(ship.id()),
            door: Door::Moving {
                to: 1.0,
                speed: 0.5,
            },
            scratch: vec![42],
        });
    let _ = crate_;
    let ha = host(&a);
    let doc = ha.encode(&Selection::Tagged, &Encode::default()).unwrap();
    let tree = ha.document(&Selection::Tagged, &Encode::default()).unwrap();
    assert_eq!(doc, serde_ipld_dagcbor::to_vec(&tree).unwrap());

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
    assert_eq!(r.entities.len(), 2);
    assert_eq!(
        hb.encode(&Selection::Tagged, &Encode::default()).unwrap(),
        doc
    );
    let ship_b = b.lookup("ship");
    ship_b.get::<&Heat>(|h| assert_eq!(*h, Heat(3.5)));
    let (_, e) = r.entities.iter().find(|(id, _)| id[15] == 2).unwrap();
    let e = b.entity_from_id(*e);
    e.get::<(&Pose, &Hold)>(|(p, h)| {
        assert_eq!(p.at, glam::Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(h.items, vec![(7, 0.25), (9, 1.5)]);
        assert_eq!(h.owner, Some(ship_b.id()));
        assert_eq!(
            h.door,
            Door::Moving {
                to: 1.0,
                speed: 0.5
            }
        );
        assert!(h.scratch.is_empty(), "a runtime field restores its default");
    });
}
