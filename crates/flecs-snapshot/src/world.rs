//! A Flecs world: run scripts in it, then read it back as a document or as blocks.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::sync::Once;

use flecs_c as sys;
use ipld_core::ipld::Ipld;
use sys::{ecs_entity_t, ecs_world_t};

use crate::dag::{self, Blocks};
use crate::schema::{self, Schema};
use crate::walk::{self, Walk};

thread_local! {
    /// Error lines Flecs logged during the current call (a world lives on one thread).
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Flecs's log hook. Parser errors arrive as level -3 with the script name as `file`, line 0, and a message starting
/// with `LINE: `; those become `file: line: msg`. Errors from Flecs's own C (line > 0) keep only the message.
unsafe extern "C" fn log(level: i32, file: *const c_char, line: i32, msg: *const c_char) {
    if level > -3 || msg.is_null() {
        return;
    }
    // SAFETY: Flecs passes NUL-terminated strings (or null for `file`).
    let msg = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let file = if file.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(file) }
            .to_string_lossy()
            .into_owned()
    };
    let first = msg.lines().next().unwrap_or("").trim_end();
    let digits = first.bytes().take_while(u8::is_ascii_digit).count();
    let text = if line == 0 && digits > 0 && first[digits..].starts_with(':') {
        format!(
            "{file}: {}: {}",
            &first[..digits],
            first[digits + 1..].trim_start()
        )
    } else if line == 0 && !file.is_empty() {
        format!("{file}: {first}")
    } else {
        first.to_string()
    };
    LOG.with(|l| l.borrow_mut().push(text));
}

/// Flecs's abort hook: an internal invariant failed, so the world is unusable. Natively, print what was logged and
/// abort; on wasm, trap.
unsafe extern "C" fn abort() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        LOG.with(|l| {
            for line in l.borrow().iter() {
                eprintln!("flecs: {line}");
            }
        });
        std::process::abort();
    }
    #[cfg(target_arch = "wasm32")]
    core::arch::wasm32::unreachable();
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// Calls a "fill up to cap, return the count" C function until the buffer is big enough.
pub(crate) fn grow(mut f: impl FnMut(*mut ecs_entity_t, i32) -> i32) -> Vec<ecs_entity_t> {
    let mut cap = 64usize;
    loop {
        let mut v = vec![0; cap];
        let n = f(v.as_mut_ptr(), cap as i32);
        if n < 0 {
            return Vec::new();
        }
        if n as usize <= cap {
            v.truncate(n as usize);
            return v;
        }
        cap = n as usize;
    }
}

pub struct World {
    w: *mut ecs_world_t,
    schema: Schema,
}

impl World {
    pub fn new() -> Result<World, String> {
        static INIT: Once = Once::new();
        // SAFETY: sets Flecs's process-wide OS hooks once, before any world exists.
        INIT.call_once(|| unsafe { sys::glue_os_api(log, abort) });
        take_log();
        // SAFETY: plain constructor.
        let w = unsafe { sys::ecs_init() };
        if w.is_null() {
            return Err("ecs_init failed".into());
        }
        if let Err(e) = script(w, "sync", schema::SCRIPT) {
            // SAFETY: `w` is ours.
            unsafe { sys::ecs_fini(w) };
            return Err(format!("defining the sync module: {e}"));
        }
        let schema = Schema::new(w)?;
        Ok(World { w, schema })
    }

    /// Runs a Flecs script. `name` is the script's name in error messages and in the stable ids it gives new entities.
    pub fn run(&mut self, name: &str, code: &str) -> Result<(), String> {
        script(self.w, name, code)?;
        self.assign_ids(name)
    }

    /// Gives every user entity (not under `flecs` or `sync`) without a stable id the one its path derives.
    fn assign_ids(&mut self, script: &str) -> Result<(), String> {
        let w = self.w;
        let mut stack: Vec<(ecs_entity_t, String)> = Vec::new();
        // SAFETY: `buf` has room for `cap` ids.
        let roots = grow(|buf, cap| unsafe { sys::glue_roots(w, buf, cap) });
        if roots.is_empty() {
            return Err(format!(
                "listing the root entities failed: {}",
                take_log().join("; ")
            ));
        }
        let s = &self.schema;
        let walk = Walk::new(w, s);
        for r in roots {
            // SAFETY: plain query.
            let name = unsafe { sys::ecs_get_name(w, r) };
            if name.is_null() || r == s.module {
                continue;
            }
            // SAFETY: as above.
            let name = unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned();
            if name == "flecs" {
                continue;
            }
            stack.push((r, walk::escape(&name)));
        }
        while let Some((e, path)) = stack.pop() {
            if s.stable_id(w, e).is_none() {
                s.set_stable_id(w, e, schema::derive_id(script, &path));
            }
            for (seg, c) in walk.children(e) {
                stack.push((c, format!("{path}.{seg}")));
            }
        }
        Ok(())
    }

    /// The world under `roots` as one DAG-CBOR document (SPEC.md, "Document").
    pub fn document(&self, roots: &[&str]) -> Result<Ipld, String> {
        let snap = Walk::new(self.w, &self.schema).run(roots)?;
        Ok(dag::document(snap))
    }

    pub fn document_bytes(&self, roots: &[&str]) -> Result<Vec<u8>, String> {
        Ok(walk::encode(&self.document(roots)?))
    }

    /// The world under `roots` as blocks: entity blocks linked by CID under one root block (SPEC.md, "Blocks").
    pub fn blocks(&self, roots: &[&str]) -> Result<Blocks, String> {
        let snap = Walk::new(self.w, &self.schema).run(roots)?;
        Ok(dag::blocks(snap))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // SAFETY: `w` is ours and dropped once.
        unsafe { sys::ecs_fini(self.w) };
    }
}

fn script(w: *mut ecs_world_t, name: &str, code: &str) -> Result<(), String> {
    let n = CString::new(name).map_err(|_| "script name contains a NUL byte".to_string())?;
    let c = CString::new(code).map_err(|_| format!("{name}: script contains a NUL byte"))?;
    take_log();
    // SAFETY: NUL-terminated strings; no result out-param.
    let rc = unsafe { sys::ecs_script_run(w, n.as_ptr(), c.as_ptr(), std::ptr::null_mut()) };
    let log = take_log();
    if rc == 0 {
        return Ok(());
    }
    if log.is_empty() {
        Err(format!("{name}: script failed"))
    } else {
        Err(log.join("\n"))
    }
}
