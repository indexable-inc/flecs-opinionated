//! Attaching a description to a component ([`register`]) and making a component with Rust's hooks ([`component`]).

use std::ffi::{CStr, CString, c_void};

use flecs_c as sys;
use flecs_snapshot::described::{Described, Refs, Resolve, describe};
use sys::{ecs_entity_t, ecs_type_info_t, ecs_world_t};

use crate::meta::{self, Meta};
use crate::{Describe, Ipld, R};

/// A path segment as flecs-snapshot writes it: `\` and `.` escaped with `\`, and a leading `#` too.
fn escape(name: &str) -> String {
    let e = name.replace('\\', "\\\\").replace('.', "\\.");
    if e.starts_with('#') {
        format!("\\{e}")
    } else {
        e
    }
}

/// An entity's path as flecs-snapshot writes it (SPEC.md, "Values"): its escaped names from the root, `.`-separated.
/// An unnamed entity on the way has no such path, which is an error.
///
/// # Safety
///
/// `world` is live.
pub unsafe fn path_of(world: *const ecs_world_t, e: ecs_entity_t) -> R<String> {
    let mut segs = Vec::new();
    let mut cur = e;
    while cur != 0 {
        if segs.len() > 1024 {
            return Err(format!(
                "entity #{} is nested too deep for a path",
                e & 0xffff_ffff
            ));
        }
        // SAFETY: plain queries on a live world.
        let name = unsafe { sys::ecs_get_name(world, cur) };
        if name.is_null() {
            return Err(format!(
                "entity #{} has no name, so it has no path",
                cur & 0xffff_ffff
            ));
        }
        // SAFETY: Flecs's names are NUL-terminated.
        segs.push(escape(&unsafe { CStr::from_ptr(name) }.to_string_lossy()));
        // SAFETY: as above.
        cur = unsafe { sys::ecs_get_parent(world, cur) };
    }
    segs.reverse();
    Ok(segs.join("."))
}

/// Replaces the top-level type name of a layout (struct, union, enum, bitmask, runtime) with `path`.
fn rename_top(l: &mut Ipld, path: &str) {
    if let Ipld::Map(m) = l {
        for k in ["struct", "union", "enum", "bitmask", "runtime"] {
            if let Some(v) = m.get_mut(k) {
                *v = Ipld::String(path.to_string());
                return;
            }
        }
    }
}

unsafe fn read_fn<T: Describe>(ptr: *const u8, refs: &mut dyn Refs) -> R<Ipld> {
    // SAFETY: the library calls this with a live value of the described type, a T.
    unsafe { &*ptr.cast::<T>() }.read(refs)
}

unsafe fn write_fn<T: Describe>(v: &Ipld, ptr: *mut u8, ids: &mut dyn Resolve) -> R<()> {
    let x = T::from_value(v, ids)?;
    // SAFETY: the library calls this with a live (constructed) T; the assignment drops the old value.
    unsafe { *ptr.cast::<T>() = x };
    Ok(())
}

/// Describes component (or type) `component` as `T`: checks its size, gives it Flecs meta or checks the meta it has,
/// and attaches the description (`flecs_snapshot::described::describe`) with the layout's top-level name replaced by
/// the component's path.
///
/// Flecs meta, when the type has none:
/// - when Flecs can express `T` with its real memory layout (`Flecs::expressible`: primitives, entities, `[T; N]`,
///   unit enums with an integer `#[repr]`, 32-bit bitflags, structs of those): a Flecs struct (array, enum,
///   bitmask) with Rust's offsets, its member types made under `flecs.rust.described`. Zero-sized members hold no
///   bytes and are left out;
/// - otherwise an opaque type whose serialize callback writes the value `T::read` gives, so Flecs's REST and JSON
///   show it. Writing it through Flecs (a script, the meta cursor, REST) is not supported: it has no assign callbacks.
///
/// When the type already has Flecs meta (flecs_ecs's `#[flecs(meta)]`, a script's struct), nothing is registered:
/// its member names, order, offsets, primitive kinds, enum and bitmask constants and array counts are checked against
/// `T` and the first mismatch is the error. A part either side holds as opaque is not looked into.
///
/// A zero-sized `T` (a tag) is only described.
///
/// # Safety
///
/// `world` is a live world not used by another thread during the call; `component`'s values, if it has any, are
/// values of `T` (made by T's constructor and dropped as T: the type's hooks are Rust's), for as long as the world.
pub unsafe fn register<T: Describe>(world: *mut ecs_world_t, component: ecs_entity_t) -> R<()> {
    // SAFETY: plain query.
    if component == 0 || !unsafe { sys::ecs_is_alive(world, component) } {
        return Err(format!(
            "registering {}: #{component} is not a live entity",
            T::type_name()
        ));
    }
    // SAFETY: the caller's contract.
    let path = unsafe { path_of(world, component) }?;
    let what = || format!("component {path} (Rust type {})", T::type_name());
    let (size, align) = (core::mem::size_of::<T>(), core::mem::align_of::<T>());
    match meta::component_size(world, component) {
        Some((s, a)) if s == size && (s == 0 || a == align) => {}
        None if size == 0 => {}
        Some((s, a)) => {
            return Err(format!(
                "{}: Flecs has size {s} and alignment {a}, Rust {size} and {align}",
                what()
            ));
        }
        None => return Err(format!("{}: not a Flecs component", what())),
    }
    let flecs = T::flecs();
    let mut layout = T::layout();
    if size > 0 {
        if meta::has_meta(world, component) {
            meta::check(world, component, &flecs, "").map_err(|e| {
                format!(
                    "{}: its Flecs meta does not match the Rust type: {e}",
                    what()
                )
            })?;
        } else {
            // SAFETY: the caller's contract.
            let mut m = unsafe { Meta::new(world) }.map_err(|e| format!("{}: {e}", what()))?;
            if !m
                .init_component(component, &flecs)
                .map_err(|e| format!("{}: {e}", what()))?
            {
                m.init_opaque::<T>(component, &layout)
                    .map_err(|e| format!("{}: {e}", what()))?;
            }
        }
    }
    rename_top(&mut layout, &path);
    let d = Described {
        layout,
        read: read_fn::<T>,
        write: write_fn::<T>,
    };
    // SAFETY: the caller's contract; read_fn and write_fn handle values of T, which `component`'s values are.
    unsafe { describe(world, component, d) }
}

unsafe extern "C" fn ctor<T: Default>(ptr: *mut c_void, count: i32, _ti: *const ecs_type_info_t) {
    let p = ptr.cast::<T>();
    for i in 0..count as usize {
        // SAFETY: Flecs gives `count` slots of uninitialised memory sized and aligned for T.
        unsafe { p.add(i).write(T::default()) };
    }
}

unsafe extern "C" fn dtor<T>(ptr: *mut c_void, count: i32, _ti: *const ecs_type_info_t) {
    let p = ptr.cast::<T>();
    for i in 0..count as usize {
        // SAFETY: `count` live values, not used again.
        unsafe { core::ptr::drop_in_place(p.add(i)) };
    }
}

/// Copy assignment: both live.
unsafe extern "C" fn copy<T: Clone>(
    dst: *mut c_void,
    src: *const c_void,
    count: i32,
    _ti: *const ecs_type_info_t,
) {
    let (d, s) = (dst.cast::<T>(), src.cast::<T>());
    for i in 0..count as usize {
        // SAFETY: `count` live values on each side, not overlapping.
        unsafe { (*d.add(i)).clone_from(&*s.add(i)) };
    }
}

/// Copy construction: `dst` uninitialised.
unsafe extern "C" fn copy_ctor<T: Clone>(
    dst: *mut c_void,
    src: *const c_void,
    count: i32,
    _ti: *const ecs_type_info_t,
) {
    let (d, s) = (dst.cast::<T>(), src.cast::<T>());
    for i in 0..count as usize {
        // SAFETY: as above, `dst` uninitialised.
        unsafe { d.add(i).write((*s.add(i)).clone()) };
    }
}

/// Move assignment: both live; `src` stays a valid value (the old `dst`), as Flecs destroys or reuses it later.
unsafe extern "C" fn move_<T>(
    dst: *mut c_void,
    src: *mut c_void,
    count: i32,
    _ti: *const ecs_type_info_t,
) {
    let (d, s) = (dst.cast::<T>(), src.cast::<T>());
    for i in 0..count as usize {
        // SAFETY: `count` live values on each side, not overlapping.
        unsafe { core::ptr::swap(d.add(i), s.add(i)) };
    }
}

/// Move construction: `dst` uninitialised; `src` stays live (left holding the default).
unsafe extern "C" fn move_ctor<T: Default>(
    dst: *mut c_void,
    src: *mut c_void,
    count: i32,
    _ti: *const ecs_type_info_t,
) {
    let (d, s) = (dst.cast::<T>(), src.cast::<T>());
    for i in 0..count as usize {
        // SAFETY: `dst` uninitialised, `src` live.
        unsafe { d.add(i).write(core::ptr::replace(s.add(i), T::default())) };
    }
}

/// Construct `dst` from `src` and destroy `src`: a bitwise move, `src` is dead afterwards.
unsafe extern "C" fn ctor_move_dtor<T>(
    dst: *mut c_void,
    src: *mut c_void,
    count: i32,
    _ti: *const ecs_type_info_t,
) {
    let (d, s) = (dst.cast::<T>(), src.cast::<T>());
    for i in 0..count as usize {
        // SAFETY: `dst` uninitialised, `src` live and not used again.
        unsafe { d.add(i).write(s.add(i).read()) };
    }
}

/// Move-assign into a live `dst` and destroy `src`.
unsafe extern "C" fn move_dtor<T>(
    dst: *mut c_void,
    src: *mut c_void,
    count: i32,
    _ti: *const ecs_type_info_t,
) {
    let (d, s) = (dst.cast::<T>(), src.cast::<T>());
    for i in 0..count as usize {
        // SAFETY: both live; `src` not used again. The assignment drops the old `dst`.
        unsafe { *d.add(i) = s.add(i).read() };
    }
}

/// Makes a Flecs component at `path` (`.`-separated, made when missing) holding values of `T`, with Rust's lifecycle
/// hooks (constructor `T::default()`, destructor `drop`, copies by `Clone`, moves by Rust moves), then
/// [`register`]s `T` on it. For tests and for hosts that do not register components through flecs_ecs. A zero-sized
/// `T` is a tag: the entity is made with no size and only described.
///
/// # Safety
///
/// `world` is a live world not used by another thread during the call.
pub unsafe fn component<T: Describe + Default + Clone>(
    world: *mut ecs_world_t,
    path: &str,
) -> R<ecs_entity_t> {
    let c = CString::new(path).map_err(|_| format!("the path {path:?} holds a NUL byte"))?;
    let (size, align) = (core::mem::size_of::<T>(), core::mem::align_of::<T>());
    let e = if size == 0 {
        // SAFETY: the caller's contract; a lookup by path, then a plain entity named by it.
        unsafe {
            let e = sys::ecs_lookup(world, c.as_ptr());
            if e != 0 {
                e
            } else {
                let e = sys::ecs_new(world);
                sys::ecs_set_name(world, e, c.as_ptr());
                e
            }
        }
    } else {
        let (size, align) = (
            i32::try_from(size).map_err(|_| format!("{path}: too large for a Flecs component"))?,
            align as i32,
        );
        // SAFETY: the caller's contract; the hooks handle values of T in slots of T's size and alignment.
        unsafe {
            sys::glue_describe_component(
                world,
                c.as_ptr(),
                size,
                align,
                ctor::<T>,
                dtor::<T>,
                copy::<T>,
                move_::<T>,
                copy_ctor::<T>,
                move_ctor::<T>,
                ctor_move_dtor::<T>,
                move_dtor::<T>,
            )
        }
    };
    if e == 0 {
        return Err(format!("Flecs refused the component {path}"));
    }
    // SAFETY: the caller's contract; `e`'s values are T, made and dropped by the hooks above.
    unsafe { register::<T>(world, e) }?;
    Ok(e)
}
