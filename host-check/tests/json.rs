//! Described opaque components through Flecs's JSON (what REST serves): a component shaped like the space game's
//! planet atlas (`Atlas(Arc<Texels>)`, a newtype over a shared struct holding a `Vec<f32>`: map, struct, list of f32)
//! through `ecs_ptr_to_json` and `ecs_world_to_json`.
//!
//! Regression: `GET /world` on the game server aborted in `flecs_strbuf_grow` (`assert(b->content != NULL)`): two
//! atlases of 26 million f32 each wrote more than 2 GiB of JSON into one `int32_t`-sized string buffer. A described
//! value now writes at most `flecs_describe::JSON_ELEMENTS` elements; a longer list is one string that says so.

use std::ffi::CStr;
use std::sync::Arc;

use flecs_describe::{Describe, JSON_ELEMENTS};
use flecs_ecs::prelude::*;
use flecs_ecs::sys;

#[derive(Describe, Clone, Debug, PartialEq)]
pub struct Texels {
    pub cells: u32,
    pub recipe: u64,
    pub data: Vec<f32>,
}

#[derive(Component, Describe, Clone, Debug)]
pub struct Atlas(pub Arc<Texels>);

fn world() -> (World, u64) {
    let w = World::new();
    let atlas = *w.component::<Atlas>().id();
    // SAFETY: the component holds values of Atlas, made and dropped by flecs_ecs's hooks.
    unsafe { flecs_describe::register::<Atlas>(w.ptr_mut().cast(), atlas) }.unwrap();
    (w, atlas)
}

/// Takes a string Flecs allocated, or panics on NULL (the serializer failed).
fn take(p: *mut core::ffi::c_char) -> String {
    assert!(!p.is_null(), "Flecs refused to serialise");
    // SAFETY: Flecs returns a NUL-terminated string it allocated with its os api.
    let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_owned();
    // SAFETY: as above; freed once with the function that allocated it.
    unsafe { sys::ecs_os_api.free_.expect("os api free")(p.cast()) };
    s
}

fn ptr_json(w: &World, ty: u64, v: &Atlas) -> String {
    // SAFETY: `v` is a live Atlas, the type of component `ty`.
    take(unsafe { sys::ecs_ptr_to_json(w.ptr_mut(), ty, core::ptr::from_ref(v).cast()) })
}

fn world_json(w: &World) -> String {
    // SAFETY: a live world, default options.
    take(unsafe { sys::ecs_world_to_json(w.ptr_mut(), core::ptr::null()) })
}

fn atlas(n: usize) -> Atlas {
    Atlas(Arc::new(Texels {
        cells: 512,
        recipe: 7,
        data: (0..n).map(|i| i as f32 * 0.25 - 1.0).collect(),
    }))
}

#[test]
fn atlas_shaped_value_to_json() {
    let (w, ty) = world();
    let v = Atlas(Arc::new(Texels {
        cells: 2,
        recipe: 9,
        data: vec![0.5, 1.25, -3.0],
    }));
    let one = ptr_json(&w, ty, &v);
    assert_eq!(
        one.replace(' ', ""),
        r#"{"0":{"cells":2,"data":[0.5,1.25,-3],"recipe":9}}"#,
        "{one}"
    );
    w.entity_named("planet").set(v);
    let all = world_json(&w);
    assert!(
        all.replace(' ', "")
            .contains(r#"{"0":{"cells":2,"data":[0.5,1.25,-3],"recipe":9}}"#),
        "{all}"
    );
}

#[test]
fn atlas_sized_value_is_elided_not_fatal() {
    let (w, ty) = world();
    let n = JSON_ELEMENTS + 1;
    let v = atlas(n);
    let one = ptr_json(&w, ty, &v);
    let note = format!("{n} elements, not shown");
    assert!(one.contains(&note), "{one}");
    assert!(one.contains(r#""cells":512"#), "{one}");
    assert!(one.len() < 1024, "{} bytes", one.len());

    // Several atlases on several bodies, as in the game: the world stays small and serialises.
    let empty = world_json(&w).len();
    for i in 0..4 {
        w.entity_named(&format!("planet{i}")).set(atlas(n));
    }
    let all = world_json(&w);
    assert_eq!(all.matches(&note).count(), 4, "{all}");
    assert!(all.len() - empty < 16 * 1024, "{} bytes over {empty}", all.len() - empty);

    // At the budget the list is still shown whole: the budget counts the newtype's one member and Texels's three.
    let at = ptr_json(&w, ty, &atlas(JSON_ELEMENTS - 4)).replace(' ', "");
    assert!(!at.contains("notshown"), "{}", &at[..200.min(at.len())]);
    assert!(at.contains("[-1,-0.75,-0.5"), "{}", &at[..200.min(at.len())]);
    let over = ptr_json(&w, ty, &atlas(JSON_ELEMENTS - 3));
    assert!(over.contains("not shown"), "{over}");
}
