//! The C ABI (include/world.h), the same natively and in wasm, built only with the `ffi` feature (so a crate that
//! depends on this one does not export these symbols from its own cdylib). Strings and roots are passed as pointer and
//! length; results stay owned by the world until its next call and are read with `world_output`. A null handle makes
//! every call return -1 (or null) without touching anything.

use std::ptr;

use crate::world::World;

pub struct Handle {
    world: World,
    /// The last call's output: result bytes on success, the UTF-8 error on failure.
    output: Vec<u8>,
}

/// SAFETY (all functions): `h` is null or a pointer `world_new` returned and `world_free` has not freed; byte pointers
/// are valid for their lengths.
unsafe fn bytes<'a>(p: *const u8, n: usize) -> &'a [u8] {
    if n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(p, n) }
    }
}

unsafe fn text<'a>(p: *const u8, n: usize) -> Result<&'a str, String> {
    std::str::from_utf8(unsafe { bytes(p, n) })
        .map_err(|_| "argument is not valid UTF-8".to_string())
}

fn finish(h: &mut Handle, r: Result<Vec<u8>, String>) -> i32 {
    match r {
        Ok(b) => {
            h.output = b;
            0
        }
        Err(e) => {
            h.output = e.into_bytes();
            1
        }
    }
}

/// A new world, or null if Flecs could not start one.
#[unsafe(no_mangle)]
pub extern "C" fn world_new() -> *mut Handle {
    match World::new() {
        Ok(world) => Box::into_raw(Box::new(Handle {
            world,
            output: Vec::new(),
        })),
        Err(_) => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_free(h: *mut Handle) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h) });
    }
}

/// Runs a script. 0 on success; 1 on failure, with the error in `world_output`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_run(
    h: *mut Handle,
    name: *const u8,
    name_len: usize,
    code: *const u8,
    code_len: usize,
) -> i32 {
    // SAFETY: see above; null is refused.
    let Some(h) = (unsafe { h.as_mut() }) else {
        return -1;
    };
    let r = (|| {
        let (n, c) = unsafe { (text(name, name_len)?, text(code, code_len)?) };
        h.world.run(n, c).map(|_| Vec::new())
    })();
    finish(h, r)
}

/// Roots are newline-separated paths.
unsafe fn roots<'a>(p: *const u8, n: usize) -> Result<Vec<&'a str>, String> {
    Ok(unsafe { text(p, n)? }
        .split('\n')
        .filter(|s| !s.is_empty())
        .collect())
}

/// The document form (DAG-CBOR) of the entities under `roots`, in `world_output`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_document(
    h: *mut Handle,
    roots_ptr: *const u8,
    roots_len: usize,
) -> i32 {
    // SAFETY: see above; null is refused.
    let Some(h) = (unsafe { h.as_mut() }) else {
        return -1;
    };
    let r = unsafe { roots(roots_ptr, roots_len) }.and_then(|r| h.world.document_bytes(&r));
    finish(h, r)
}

/// The block form as a CARv1 file (root block first), in `world_output`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_blocks(
    h: *mut Handle,
    roots_ptr: *const u8,
    roots_len: usize,
) -> i32 {
    // SAFETY: see above; null is refused.
    let Some(h) = (unsafe { h.as_mut() }) else {
        return -1;
    };
    let r =
        unsafe { roots(roots_ptr, roots_len) }.and_then(|r| h.world.blocks(&r).map(|b| b.car()));
    finish(h, r)
}

/// The Ephemeral parts under `roots` (World::ephemeral) as one DAG-CBOR message, in `world_output`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_ephemeral(
    h: *mut Handle,
    roots_ptr: *const u8,
    roots_len: usize,
) -> i32 {
    // SAFETY: see above; null is refused.
    let Some(h) = (unsafe { h.as_mut() }) else {
        return -1;
    };
    let r = unsafe { roots(roots_ptr, roots_len) }.and_then(|r| h.world.ephemeral_bytes(&r));
    finish(h, r)
}

/// The named root paths (World::roots), newline-separated, in `world_output`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_roots(h: *mut Handle) -> i32 {
    // SAFETY: see above; null is refused.
    let Some(h) = (unsafe { h.as_mut() }) else {
        return -1;
    };
    let r = h.world.roots().map(|r| r.join("\n").into_bytes());
    finish(h, r)
}

/// Sets `component` on the entity at `path` to a value given as DAG-CBOR (World::set).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_set(
    h: *mut Handle,
    path: *const u8,
    path_len: usize,
    component: *const u8,
    component_len: usize,
    value: *const u8,
    value_len: usize,
) -> i32 {
    // SAFETY: see above; null is refused.
    let Some(h) = (unsafe { h.as_mut() }) else {
        return -1;
    };
    let r = (|| {
        let (p, c) = unsafe { (text(path, path_len)?, text(component, component_len)?) };
        let v: ipld_core::ipld::Ipld =
            serde_ipld_dagcbor::from_slice(unsafe { bytes(value, value_len) })
                .map_err(|e| format!("the value is not DAG-CBOR: {e}"))?;
        h.world.set(p, c, &v).map(|()| Vec::new())
    })();
    finish(h, r)
}

/// The last call's output: a pointer to its bytes, with the length written to `*len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_output(h: *const Handle, len: *mut usize) -> *const u8 {
    // SAFETY: see above; null is refused.
    let Some(h) = (unsafe { h.as_ref() }) else {
        return ptr::null();
    };
    if !len.is_null() {
        // SAFETY: `len` is writable; it may be unaligned in a host's buffer.
        unsafe { len.write_unaligned(h.output.len()) };
    }
    h.output.as_ptr()
}

/// Wasm hosts allocate argument buffers in the module's memory with these (natively, use your own memory).
/// `buf_alloc(0)` returns a valid one-byte buffer; pass the same `n` to `buf_free`, which ignores null.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn buf_alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn buf_free(p: *mut u8, n: usize) {
    if p.is_null() {
        return;
    }
    // SAFETY: `p` came from buf_alloc(n), which reserved n.max(1) bytes.
    drop(unsafe { Vec::from_raw_parts(p, 0, n.max(1)) });
}
