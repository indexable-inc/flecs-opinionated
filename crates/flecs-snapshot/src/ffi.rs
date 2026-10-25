//! The C ABI (include/world.h), the same natively and in wasm. Strings and roots are passed as pointer and length;
//! results stay owned by the world until its next call and are read with `world_output`.

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
    let h = unsafe { &mut *h };
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
    let h = unsafe { &mut *h };
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
    let h = unsafe { &mut *h };
    let r =
        unsafe { roots(roots_ptr, roots_len) }.and_then(|r| h.world.blocks(&r).map(|b| b.car()));
    finish(h, r)
}

/// The last call's output: a pointer to its bytes, with the length written to `*len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn world_output(h: *const Handle, len: *mut usize) -> *const u8 {
    let h = unsafe { &*h };
    unsafe { *len = h.output.len() };
    h.output.as_ptr()
}

/// Wasm hosts allocate argument buffers in the module's memory with these (natively, use your own memory).
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
    drop(unsafe { Vec::from_raw_parts(p, 0, n.max(1)) });
}
