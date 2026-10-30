//! Host mode with Flecs's non-fragmenting hierarchy and DontFragment storage (SPEC.md "Host record"): `Parent`
//! children in an order that is not their creation or id order, a DontFragment exclusive pair and a DontFragment
//! component value, saved, restored and re-encoded to the same bytes.

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

/// A DontFragment exclusive relationship: the chunk a part is in.
#[derive(Component, Default, Clone, Copy)]
pub struct InChunk;

/// A DontFragment component with a value.
#[derive(Component, Default, Clone, Copy)]
#[flecs(meta)]
pub struct Heat {
    pub value: f32,
}

const SHIP: u64 = 100;

fn register(w: &World) {
    w.component::<Uid>();
    w.component::<Persist>();
    // UNSURE(integrator): add_trait::<flecs::DontFragment>() / ::<flecs::Exclusive>() on a Component handle, as in
    // component_traits.rs's doc example; both traits must be added before the first use of the id.
    w.component::<InChunk>()
        .add_trait::<flecs::DontFragment>()
        .add_trait::<flecs::Exclusive>();
    w.component::<Heat>().add_trait::<flecs::DontFragment>();
}

fn host(w: &World) -> Host {
    // SAFETY: the World outlives the Host, and the test is single-threaded per world.
    unsafe { Host::new(w.ptr_mut().cast(), "Uid", "Persist") }.unwrap()
}

/// The saved children of `parent` in Flecs's order, as their Uid `lo`.
fn order(w: &World, parent: Entity) -> Vec<u64> {
    let mut out = Vec::new();
    // UNSURE(integrator): each_child walks Flecs's ordered children for a Parent-storage parent (ecs_children
    // covers both storages on master); if it does not, read the order from the encoded `children` instead.
    w.entity_from_id(parent).each_child(|c| {
        if c.has(Uid::id()) {
            c.get::<&Uid>(|u| out.push(u.lo));
        }
    });
    out
}

fn find(w: &World, entities: &[([u8; 16], u64)], lo: u64) -> Entity {
    entities
        .iter()
        .map(|&(_, e)| Entity::new(e))
        .find(|&e| {
            let v = w.entity_from_id(e);
            v.has(Uid::id()) && v.get::<&Uid>(|u| u.lo) == lo
        })
        .expect("restored entity")
}

#[test]
fn parent_children_and_dont_fragment_round_trip() {
    let a = World::new();
    register(&a);
    let chunk = a.entity().set(Uid { hi: 7, lo: 1 }).add(Persist::id());
    let ship = a
        .entity_named("ship")
        .set(Uid { hi: 7, lo: SHIP })
        .add(Persist::id());
    // Created 30, 10, 20, so neither creation order nor Uid order is the order set below.
    // UNSURE(integrator): World::entity_with_parent (Parent storage) as in crates/flecs_ecs world/entity_view.rs.
    let p30 = a
        .entity_with_parent(ship)
        .set(Uid { hi: 7, lo: 30 })
        .add(Persist::id());
    let p10 = a
        .entity_with_parent(ship)
        .set(Uid { hi: 7, lo: 10 })
        .add(Persist::id());
    let p20 = a
        .entity_with_parent(ship)
        .set(Uid { hi: 7, lo: 20 })
        .add(Persist::id());
    // UNSURE(integrator): a DontFragment pair added with the tuple IntoId, and set() of a DontFragment component.
    p10.add((id::<InChunk>(), chunk));
    p30.set(Heat { value: 2.5 });
    ship.set_child_order(&[*p20, *p30, *p10]);
    assert_eq!(order(&a, *ship), vec![20, 30, 10]);

    let mut ha = host(&a);
    let (sel, opts) = (Selection::Tagged, Encode::default());
    let doc = ha.encode(&sel, &opts).unwrap();
    let tree = ha.document(&sel, &opts).unwrap();
    assert_eq!(
        doc,
        serde_ipld_dagcbor::to_vec(&tree).unwrap(),
        "the direct writer matches the Ipld path"
    );
    flecs_snapshot::check::host_document(&tree).unwrap();

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
    assert_eq!(r.entities.len(), 5);
    assert_eq!(hb.encode(&sel, &opts).unwrap(), doc, "re-encode is byte-identical");

    let ship_b = find(&b, &r.entities, SHIP);
    assert_eq!(order(&b, ship_b), vec![20, 30, 10], "child order survives");
    let p10_b = b.entity_from_id(find(&b, &r.entities, 10));
    let chunk_b = find(&b, &r.entities, 1);
    // UNSURE(integrator): EntityView::target on a DontFragment exclusive relationship.
    assert_eq!(p10_b.target(id::<InChunk>(), 0).map(|t| *t), Some(chunk_b));
    assert_eq!(
        b.entity_from_id(find(&b, &r.entities, 30)).get::<&Heat>(|h| h.value),
        2.5
    );

    // Incremental blocks see a reorder and a DontFragment value change, though neither moves a table.
    let full = ha.blocks_full(&sel, &opts).unwrap();
    assert_eq!(ha.blocks(&sel, &opts).unwrap().root, full.root);
    ship.set_child_order(&[*p10, *p20, *p30]);
    let reordered = ha.blocks(&sel, &opts).unwrap();
    assert_ne!(reordered.root, full.root, "a reorder changes the root");
    assert_eq!(reordered.root, ha.blocks_full(&sel, &opts).unwrap().root);
    p30.set(Heat { value: 9.0 });
    let heated = ha.blocks(&sel, &opts).unwrap();
    assert_ne!(heated.root, reordered.root, "a DontFragment value change changes the root");
    assert_eq!(heated.root, ha.blocks_full(&sel, &opts).unwrap().root);
}
