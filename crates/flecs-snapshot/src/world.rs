//! A Flecs world: run scripts in it, then read it back as a document or as blocks.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString, c_char};
use std::sync::Once;

use flecs_c as sys;
use ipld_core::ipld::Ipld;
use sys::{ecs_entity_t, ecs_world_t};

use crate::check::{self, Blobs};
use crate::dag::{self, Blocks};
use crate::described::Resolve;
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
    /// Stable id to entity, built when `set` first needs it and dropped by `run` (which may add entities).
    ids: Option<HashMap<[u8; 16], ecs_entity_t>>,
}

impl World {
    /// A new world with the `sync` module defined.
    ///
    /// Flecs's OS hooks (log, abort) are process-wide and set by the first call; the log they fill is per thread. A
    /// world is used only on the thread that made it (it is not `Send`), and every world in a process should be made on
    /// one thread, or a Flecs error raised while two threads run worlds can land in the other thread's log.
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
        if let Err(e) = schema::define(w) {
            // SAFETY: `w` is ours.
            unsafe { sys::ecs_fini(w) };
            return Err(format!("defining the sync module: {e}"));
        }
        let schema = match Schema::new(w) {
            Ok(s) => s,
            Err(e) => {
                // SAFETY: `w` is ours and not used again.
                unsafe { sys::ecs_fini(w) };
                return Err(e);
            }
        };
        Ok(World {
            w,
            schema,
            ids: None,
        })
    }

    /// Runs a Flecs script. `name` is the script's name in error messages and in the stable ids it gives new entities.
    pub fn run(&mut self, name: &str, code: &str) -> Result<(), String> {
        self.ids = None;
        // A failed script may have made entities before it stopped: they get ids too, so the world never holds a
        // user entity without one.
        let ran = script(self.w, name, code);
        let ids = self.assign_ids(name);
        ran?;
        ids
    }

    /// The paths of every named root entity, prefabs included, without Flecs's own (`flecs`) and the `sync` module,
    /// sorted (bytewise). Pass them to `document` or `blocks` to snapshot the whole world.
    pub fn roots(&self) -> Result<Vec<String>, String> {
        let w = self.w;
        // SAFETY: `buf` has room for `cap` ids.
        let roots = grow(|buf, cap| unsafe { sys::glue_roots(w, buf, cap) });
        let mut out = Vec::new();
        for r in roots {
            if r == self.schema.module {
                continue;
            }
            // SAFETY: plain query; Flecs returns a NUL-terminated name or null.
            let name = unsafe { sys::ecs_get_name(w, r) };
            if name.is_null() {
                continue;
            }
            // SAFETY: as above.
            let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
            if name != "flecs" {
                out.push(walk::escape(&name));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Sets component `component` (a type path, such as `Title`) on the entity at `path` to `value`. The value has the
    /// snapshot shape of the component's layout (SPEC.md, "Values") and is checked against it first: a struct needs
    /// exactly its members, integers must fit, a ref must name an existing entity. A Blob field takes the bytes, not a
    /// link. The entity and the component must exist; nothing is created. The old value is replaced.
    pub fn set(&mut self, path: &str, component: &str, value: &Ipld) -> Result<(), String> {
        let w = self.w;
        let e = self.lookup(path)?;
        let c = self.lookup(component)?;
        let s = &self.schema;
        if c == s.id_component {
            return Err("sync.Id is set by the library, never directly".into());
        }
        let mut walk = Walk::new(w, s);
        let size = walk
            .component_size(c)
            .map_err(|e| format!("{component}: {e}"))?;
        let layout = walk.layout(c).map_err(|e| format!("{component}: {e}"))?;
        check::value(value, &layout, Blobs::Bytes)
            .map_err(|e| format!("{path}: {component}: {e}"))?;
        if needs_ids(value) && self.ids.is_none() {
            self.ids = Some(self.index_ids()?);
        }
        let mut ids = ScriptIds {
            w,
            s,
            ids: self.ids.as_ref(),
        };
        take_log();
        let r = make_value(w, s, c, value, &layout, &mut ids).map(|v| {
            // SAFETY: `v` holds a value of type `c` of `size` bytes; ecs_set_id copies it with the type's hooks.
            unsafe { sys::glue_writer_commit(w, v.0, e, c, size) }
        });
        let log = take_log();
        r.map_err(|m| format!("{path}: {component}: {m}"))?;
        if !log.is_empty() {
            return Err(format!("{path}: {component}: {}", log.join("; ")));
        }
        Ok(())
    }

    /// The layout record (SPEC.md, "Layouts") of the type at path `ty`, as a document holds it: the shape `set`
    /// checks a value of it against, and what `compare` classifies a change of. A writer that holds values in
    /// another encoding (JSON has no integer/float distinction) reads it to type them before `set`.
    pub fn layout_record(&self, ty: &str) -> Result<Ipld, String> {
        let t = self.lookup(ty)?;
        Walk::new(self.w, &self.schema).layout_record_of(t)
    }

    /// The world's raw pointer, for describing types in it (`crate::described::describe`) and for tests.
    #[doc(hidden)]
    pub fn as_ptr(&mut self) -> *mut ecs_world_t {
        self.w
    }

    /// Renames an entity with Flecs's C API, which (unlike a script) takes any name, `#0` included.
    #[cfg(test)]
    pub(crate) fn rename(&mut self, path: &str, name: &str) -> Result<(), String> {
        let e = self.lookup(path)?;
        let c = CString::new(name).map_err(|_| "NUL in name")?;
        // SAFETY: plain call with a NUL-terminated name.
        unsafe { sys::ecs_set_name(self.w, e, c.as_ptr()) };
        Ok(())
    }

    /// The entity at a snapshot path (escaped segments, `#n` for unnamed children).
    fn lookup(&self, path: &str) -> Result<ecs_entity_t, String> {
        Walk::new(self.w, &self.schema).resolve(path)
    }

    /// Runs a walk with the log drained before and after, so its Flecs errors are reported here and never reach a
    /// later call.
    fn snapshot(&self, roots: &[&str]) -> Result<walk::Snapshot, String> {
        take_log();
        let r = Walk::new(self.w, &self.schema).run(roots);
        let log = take_log();
        match r {
            Err(e) if !log.is_empty() => Err(format!("{e} ({})", log.join("; "))),
            r => r,
        }
    }

    /// Every user entity's stable id. A hash map: lookups are by exact id, never by range.
    fn index_ids(&self) -> Result<HashMap<[u8; 16], ecs_entity_t>, String> {
        let w = self.w;
        let s = &self.schema;
        let walk = Walk::new(w, s);
        let mut out = HashMap::new();
        // SAFETY: `buf` has room for `cap` ids.
        let mut stack = grow(|buf, cap| unsafe { sys::glue_roots(w, buf, cap) });
        while let Some(e) = stack.pop() {
            if let Some(id) = s.stable_id(w, e) {
                out.insert(id, e);
            }
            stack.extend(walk.children(e).into_iter().map(|(_, c)| c));
        }
        Ok(out)
    }

    /// The Ephemeral parts under `roots` (SPEC.md, "Sync modes"): what a presence or cursor channel sends. These are
    /// never in `document` or `blocks`.
    pub fn ephemeral(&self, roots: &[&str]) -> Result<Ipld, String> {
        let snap = self.snapshot(roots)?;
        Ok(dag::ephemeral(snap))
    }

    pub fn ephemeral_bytes(&self, roots: &[&str]) -> Result<Vec<u8>, String> {
        Ok(walk::encode(&self.ephemeral(roots)?))
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
        let mut seen: HashMap<[u8; 16], String> = HashMap::new();
        let mut clash = None;
        while let Some((e, path)) = stack.pop() {
            let id = match s.stable_id(w, e) {
                Some(id) => id,
                None => {
                    let id = schema::derive_id(script, &path);
                    s.set_stable_id(w, e, id);
                    id
                }
            };
            if let Some(other) = seen.insert(id, path.clone())
                && clash.is_none()
            {
                clash = Some((other, path.clone()));
            }
            for (seg, c) in walk.children(e) {
                stack.push((c, format!("{path}.{seg}")));
            }
        }
        // Every entity gets its id before this reports, so one clash does not leave others without ids.
        match clash {
            Some((a, b)) => {
                let (a, b) = if a < b { (a, b) } else { (b, a) };
                Err(format!("{a} and {b} have the same stable id"))
            }
            None => Ok(()),
        }
    }

    /// The world under `roots` as one DAG-CBOR document (SPEC.md, "Document").
    pub fn document(&self, roots: &[&str]) -> Result<Ipld, String> {
        let snap = self.snapshot(roots)?;
        Ok(dag::document(snap))
    }

    pub fn document_bytes(&self, roots: &[&str]) -> Result<Vec<u8>, String> {
        Ok(walk::encode(&self.document(roots)?))
    }

    /// The world under `roots` as blocks: entity blocks linked by CID under one root block (SPEC.md, "Blocks").
    pub fn blocks(&self, roots: &[&str]) -> Result<Blocks, String> {
        let snap = self.snapshot(roots)?;
        Ok(dag::blocks(snap))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // SAFETY: `w` is ours and dropped once.
        unsafe { sys::ecs_fini(self.w) };
    }
}

pub(crate) fn script(w: *mut ecs_world_t, name: &str, code: &str) -> Result<(), String> {
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

fn needs_ids(v: &Ipld) -> bool {
    match v {
        Ipld::Map(m) => m.contains_key("id") || m.values().any(needs_ids),
        Ipld::List(l) => l.iter().any(needs_ids),
        _ => false,
    }
}

/// A value being made: a default-constructed value of a type, then written. Freed on drop.
pub(crate) struct Value(pub *mut sys::glue_writer_t, *const ecs_world_t);

impl Value {
    /// The value's memory.
    pub fn ptr(&self) -> *const u8 {
        // SAFETY: the writer is live while `self` is.
        unsafe { sys::glue_writer_ptr(self.0) }.cast()
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        // SAFETY: freed once.
        unsafe { sys::glue_writer_free(self.1, self.0) };
    }
}

/// A value of type `ty` holding `v`, which was checked against `layout` (the type's layout): through the type's
/// description when it has one (SPEC.md, "Described types"), else through a Flecs meta cursor.
pub(crate) fn make_value(
    w: *mut ecs_world_t,
    s: &Schema,
    ty: ecs_entity_t,
    v: &Ipld,
    layout: &Ipld,
    ids: &mut dyn Resolve,
) -> Result<Value, String> {
    // A write goes through the type's checks like a restore (SPEC.md, "Logical types").
    crate::logical::run_checks(w, v, layout)?;
    // SAFETY: a new writer over a default-constructed value of type `ty`, freed by Value's drop.
    let wr = unsafe { sys::glue_writer_new(w, ty) };
    if wr.is_null() {
        return Err("cannot make a value of it".into());
    }
    let out = Value(wr, w);
    if let Some(d) = crate::described::of(w, s.described, ty) {
        // SAFETY: the writer's value is a live, constructed value of `ty`, which `d` describes.
        unsafe { (d.write)(v, out.ptr().cast_mut(), ids)? };
        return Ok(out);
    }
    let mut cx = Writer {
        // SAFETY: `wr` is live while `out` is.
        cur: unsafe { sys::glue_writer_cursor(wr) },
        ids,
    };
    cx.write(v, layout)?;
    Ok(out)
}

/// A script world's refs: stable ids through the index `World::set` builds, paths through the snapshot's own path
/// resolution.
struct ScriptIds<'a> {
    w: *mut ecs_world_t,
    s: &'a Schema,
    ids: Option<&'a HashMap<[u8; 16], ecs_entity_t>>,
}

impl Resolve for ScriptIds<'_> {
    fn entity(&mut self, r: &Ipld) -> Result<ecs_entity_t, String> {
        let Ipld::Map(m) = r else {
            return Err("not a ref".into());
        };
        if let Some(Ipld::Bytes(id)) = m.get("id") {
            let id: [u8; 16] = id.as_slice().try_into().map_err(|_| "id is not 16 bytes")?;
            return self
                .ids
                .and_then(|ids| ids.get(&id).copied())
                .ok_or_else(|| format!("no entity has the stable id {}", hex(&id)));
        }
        if let Some(Ipld::String(p)) = m.get("path") {
            return Walk::new(self.w, self.s).resolve(p);
        }
        Err("not a ref".into())
    }
}

/// Writes a checked value through a Flecs meta cursor, following its layout.
/// Writes `v` (checked against `layout`) over the live value of `ty` at `ptr`: by its description, or through a Flecs
/// meta cursor.
pub(crate) fn write_at(
    w: *mut ecs_world_t,
    s: &Schema,
    ty: ecs_entity_t,
    ptr: *mut u8,
    v: &Ipld,
    layout: &Ipld,
    ids: &mut dyn Resolve,
) -> Result<(), String> {
    if let Some(d) = crate::described::of(w, s.described, ty) {
        // SAFETY: `ptr` is a live, constructed value of `ty`, which `d` describes.
        return unsafe { (d.write)(v, ptr, ids) };
    }
    // SAFETY: a cursor over the live value; freed below.
    let cur = unsafe { sys::glue_cursor_new(w, ty, ptr.cast()) };
    if cur.is_null() {
        return Err("cannot write a value of it".into());
    }
    let r = Writer { cur, ids }.write(v, layout);
    // SAFETY: made above, not used again.
    unsafe { sys::glue_cursor_free(cur) };
    r
}

struct Writer<'a> {
    cur: *mut sys::ecs_meta_cursor_t,
    ids: &'a mut dyn Resolve,
}

fn ok(rc: i32, what: &str) -> Result<(), String> {
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("Flecs refused {what}: {}", take_log().join("; ")))
    }
}

fn members(l: &BTreeMap<String, Ipld>) -> Vec<(&str, &Ipld)> {
    match &l["members"] {
        Ipld::List(ms) => ms
            .iter()
            .filter_map(|m| match m {
                Ipld::List(m) => match (&m[0], &m[1]) {
                    (Ipld::String(n), l) => Some((n.as_str(), l)),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The value of a named constant in an enum or bitmask layout.
fn constant(l: &BTreeMap<String, Ipld>, name: &str) -> Result<i128, String> {
    if let Ipld::List(cs) = &l["constants"] {
        for c in cs {
            if let Ipld::List(c) = c
                && let [Ipld::String(n), Ipld::Integer(v)] = c.as_slice()
                && n == name
            {
                return Ok(*v);
            }
        }
    }
    Err(format!("{name:?} is not a constant"))
}

impl Writer<'_> {
    fn entity(&mut self, r: &Ipld) -> Result<ecs_entity_t, String> {
        self.ids.entity(r)
    }

    fn bytes(&self, b: &[u8]) -> Result<(), String> {
        let c = self.cur;
        // SAFETY: `c` is a live cursor positioned on a vector of u8.
        unsafe {
            ok(sys::ecs_meta_push(c), "entering the byte vector")?;
            for (i, x) in b.iter().enumerate() {
                if i > 0 {
                    ok(sys::ecs_meta_next(c), "the next byte")?;
                }
                ok(sys::ecs_meta_set_uint(c, *x as u64), "a byte")?;
            }
            ok(sys::ecs_meta_pop(c), "leaving the byte vector")
        }
    }

    fn elements(&mut self, items: &[Ipld], l: &Ipld) -> Result<(), String> {
        let c = self.cur;
        // SAFETY: `c` is a live cursor positioned on an array or vector.
        unsafe { ok(sys::ecs_meta_push(c), "entering a collection")? };
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                // SAFETY: as above.
                unsafe { ok(sys::ecs_meta_next(c), "the next element")? };
            }
            self.write(x, l)?;
        }
        // SAFETY: as above.
        unsafe { ok(sys::ecs_meta_pop(c), "leaving a collection") }
    }

    /// `v` was checked against `l` by `check::value`, so shapes match here.
    fn write(&mut self, v: &Ipld, l: &Ipld) -> Result<(), String> {
        // A refinement is written as its physical type.
        let l = crate::logical::physical(l);
        let c = self.cur;
        match (l, v) {
            (Ipld::String(p), _) => {
                // SAFETY: `c` is a live cursor positioned on a value of primitive layout `p`.
                unsafe {
                    match (p.as_str(), v) {
                        ("bytes" | "blob", Ipld::Bytes(b)) => self.bytes(b),
                        (_, Ipld::Null) => ok(sys::ecs_meta_set_null(c), "null"),
                        (_, Ipld::Bool(b)) => ok(sys::ecs_meta_set_bool(c, *b), "a bool"),
                        (_, Ipld::Integer(i)) if *i < 0 => {
                            ok(sys::ecs_meta_set_int(c, *i as i64), "an integer")
                        }
                        (_, Ipld::Integer(i)) => {
                            ok(sys::ecs_meta_set_uint(c, *i as u64), "an integer")
                        }
                        (_, Ipld::Float(f)) => ok(sys::ecs_meta_set_float(c, *f), "a float"),
                        (_, Ipld::String(s)) => {
                            let s = CString::new(s.as_str())
                                .map_err(|_| "a string contains a NUL byte")?;
                            ok(sys::ecs_meta_set_string(c, s.as_ptr()), "a string")
                        }
                        ("id", Ipld::List(pair)) => {
                            let (r, t) = (self.entity(&pair[0])?, self.entity(&pair[1])?);
                            ok(sys::ecs_meta_set_id(c, sys::pair(r, t)), "a pair")
                        }
                        ("id", r) => ok(sys::ecs_meta_set_id(c, self.entity(r)?), "an id"),
                        (_, r) => ok(sys::ecs_meta_set_entity(c, self.entity(r)?), "an entity"),
                    }
                }
            }
            (Ipld::Map(m), Ipld::Map(vm)) if m.contains_key("struct") => {
                // SAFETY: `c` is a live cursor positioned on a struct.
                unsafe { ok(sys::ecs_meta_push(c), "entering a struct")? };
                for (name, ml) in members(m) {
                    let n = CString::new(name).map_err(|_| "member name contains a NUL byte")?;
                    // SAFETY: as above.
                    unsafe { ok(sys::ecs_meta_member(c, n.as_ptr()), "a member")? };
                    self.write(&vm[name], ml)
                        .map_err(|e| format!("{name}: {e}"))?;
                }
                // SAFETY: as above.
                unsafe { ok(sys::ecs_meta_pop(c), "leaving a struct") }
            }
            (Ipld::Map(m), Ipld::String(name)) if m.contains_key("enum") => {
                let v = constant(m, name)?;
                // SAFETY: `c` is a live cursor positioned on an enum.
                unsafe {
                    if v < 0 {
                        ok(sys::ecs_meta_set_int(c, v as i64), "an enum")
                    } else {
                        ok(sys::ecs_meta_set_uint(c, v as u64), "an enum")
                    }
                }
            }
            (Ipld::Map(m), Ipld::List(names)) if m.contains_key("bitmask") => {
                let mut bits = 0u64;
                for n in names {
                    if let Ipld::String(n) = n {
                        bits |= constant(m, n)? as u64;
                    }
                }
                // SAFETY: `c` is a live cursor positioned on a bitmask.
                unsafe { ok(sys::ecs_meta_set_uint(c, bits), "a bitmask") }
            }
            (Ipld::Map(m), Ipld::List(items)) if m.contains_key("array") => {
                self.elements(items, &m["array"])
            }
            (Ipld::Map(m), Ipld::List(items)) if m.contains_key("vector") => {
                self.elements(items, &m["vector"])
            }
            (Ipld::Map(m), _)
                if ["option", "union", "map", "runtime"].iter().any(|k| m.contains_key(*k)) =>
            {
                Err("this value is a described type's (an option, union, map or runtime value) inside a type \
                     Flecs meta describes: describe the enclosing type too"
                    .into())
            }
            _ => Err("the value does not fit its layout".into()),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
