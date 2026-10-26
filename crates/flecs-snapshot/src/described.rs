//! Described types (SPEC.md, "Described types"): a host process describes a type Flecs's reflection cannot (a Rust
//! enum with data, an `Option`, a map, a half float, a struct in Rust's own member order) with a layout and two
//! functions, and the library reads and writes its values through them instead of through Flecs meta. The
//! description is runtime data attached to the type entity (`sync.Described`), so the walk, the checker and the
//! migrations see one layout whatever produced it. `flecs-describe` writes descriptions from Rust types.

use std::ffi::CString;

use flecs_c as sys;
use ipld_core::ipld::Ipld;
use sys::{ecs_entity_t, ecs_world_t};

pub type R<T> = Result<T, String>;

/// Turns an entity (never 0; a null entity is written as null by the reader itself) into a ref (SPEC.md, "Values").
pub trait Refs {
    fn reference(&mut self, e: ecs_entity_t) -> R<Ipld>;
}

/// Turns a ref into a live entity in the world being written.
pub trait Resolve {
    fn entity(&mut self, r: &Ipld) -> R<ecs_entity_t>;
}

/// Reads the value at `ptr` (a live value of the described type) in the shape of its layout.
pub type ReadFn = unsafe fn(ptr: *const u8, refs: &mut dyn Refs) -> R<Ipld>;

/// Writes `v` (already checked against the layout) over the value at `ptr`, a live value of the described type
/// (made by its constructor); the old value is dropped by the write.
pub type WriteFn = unsafe fn(v: &Ipld, ptr: *mut u8, ids: &mut dyn Resolve) -> R<()>;

/// A type's description. `layout` is an `L` (SPEC.md, "Layouts") whose struct members' rule slots hold the rule
/// declared on that member or null; the library fills the rest as it does for a Flecs struct.
pub struct Described {
    pub layout: Ipld,
    pub read: ReadFn,
    pub write: WriteFn,
}

fn lookup(w: *const ecs_world_t, path: &str) -> ecs_entity_t {
    let c = CString::new(path).expect("no NUL");
    // SAFETY: plain lookup with a NUL-terminated path.
    unsafe { sys::ecs_lookup(w, c.as_ptr()) }
}

/// Attaches a description to type `ty`, adding the `sync` module first if the world has none. The layout is checked
/// (SPEC.md, "Versioning": well formed, known primitive names). The description is leaked: it lives as long as the
/// process, as the type does; describing a type again (after a live patch) replaces the pointer.
///
/// # Safety
///
/// `world` is a live world not used by another thread during the call; `ty` is a type entity in it whose values
/// `d.read` and `d.write` handle.
pub unsafe fn describe(world: *mut ecs_world_t, ty: ecs_entity_t, d: Described) -> R<()> {
    crate::check::described_layout(&d.layout).map_err(|e| format!("describing #{ty}: {e}"))?;
    crate::schema::ensure(world)?;
    let comp = lookup(world, "sync.Described");
    if comp == 0 {
        return Err("sync.Described missing after the sync module was added".into());
    }
    // SAFETY: plain query.
    if !unsafe { sys::ecs_is_alive(world, ty) } {
        return Err(format!("describing #{ty}: not a live entity"));
    }
    let p: &'static Described = Box::leak(Box::new(d));
    let v: usize = std::ptr::from_ref(p) as usize;
    // SAFETY: sync.Described is `{ptr: uptr}`, one pointer-sized integer.
    unsafe {
        sys::ecs_set_id(
            world,
            ty,
            comp,
            core::mem::size_of::<usize>(),
            (&raw const v).cast(),
        )
    };
    Ok(())
}

/// The description of type `ty`, if it has one. `comp` is `sync.Described` in that world (0 when absent).
pub(crate) fn of(
    w: *const ecs_world_t,
    comp: ecs_entity_t,
    ty: ecs_entity_t,
) -> Option<&'static Described> {
    if comp == 0 || ty == 0 {
        return None;
    }
    // SAFETY: plain query; sync.Described holds a pointer `describe` leaked, valid for the process.
    unsafe {
        if !sys::ecs_owns_id(w, ty, comp) {
            return None;
        }
        let p = sys::ecs_get_id(w, ty, comp).cast::<usize>();
        let p = p.as_ref()?;
        (*p as *const Described).as_ref()
    }
}

/// The description of type `ty` in a world that may not have the `sync` module.
///
/// # Safety
///
/// `world` is live.
pub unsafe fn described(world: *const ecs_world_t, ty: ecs_entity_t) -> Option<&'static Described> {
    of(world, lookup(world, "sync.Described"), ty)
}
