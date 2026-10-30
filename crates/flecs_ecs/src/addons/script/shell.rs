//! The script shell (`FLECS_SCRIPT_SHELL`): command sessions that run Flecs script shell commands.

extern crate alloc;
use alloc::{borrow::ToOwned, string::String, vec::Vec};
use core::ffi::CStr;

use crate::sys;
use flecs_ecs::core::*;

/// The outcome of [`World::command_run`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandResult {
    /// The value the command returned, as a Flecs script expression; `None` when it returned nothing or failed.
    pub value: Option<String>,
    /// Errors logged while the command ran, one entry per error.
    pub errors: Vec<String>,
}

impl CommandResult {
    /// Whether the command logged no errors.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

impl World {
    /// Create a command session (`ecs_shell_session_init`).
    ///
    /// Commands run in the session resolve identifiers relative to `scope` (0 for the root), and the session keeps
    /// the last `history_size` commands with their results (0 for Flecs's default).
    pub fn command_session(&self, scope: impl IntoEntity, history_size: i32) -> EntityView<'_> {
        let desc = sys::ecs_shell_session_desc_t {
            entity: 0,
            scope: *scope.into_entity(self),
            history_size,
        };
        // SAFETY: the world is live; desc is fully initialised.
        let session = unsafe { sys::ecs_shell_session_init(self.ptr_mut(), &desc) };
        EntityView::new_from(self, session)
    }

    /// Run one shell command line in `session` (`ecs_shell_run`).
    pub fn command_run(&self, session: impl IntoEntity, cmd: &str) -> CommandResult {
        let world = self.ptr_mut();
        let cmd = compact_str::format_compact!("{}\0", cmd);
        // SAFETY: the world is live; cmd is NUL-terminated and outlives the call.
        let mut res = unsafe {
            sys::ecs_shell_run(world, *session.into_entity(self), cmd.as_ptr() as *const _)
        };

        let value = if res.result.type_ != 0 && !res.result.ptr.is_null() {
            // SAFETY: the result value is a live value of type `type_` owned by `res`.
            let expr = unsafe { sys::ecs_ptr_to_expr(world, res.result.type_, res.result.ptr) };
            if expr.is_null() {
                None
            } else {
                // SAFETY: ecs_ptr_to_expr returns a NUL-terminated string allocated with the os api.
                let s = unsafe { CStr::from_ptr(expr) }
                    .to_string_lossy()
                    .into_owned();
                unsafe {
                    sys::ecs_os_api.free_.expect("os api is missing")(
                        expr as *mut core::ffi::c_void,
                    )
                };
                Some(s)
            }
        } else {
            None
        };

        // SAFETY: `errors` is a vec<char*> owned by `res`.
        let count = unsafe { sys::ecs_vec_count(&res.errors) };
        let first = unsafe { sys::ecs_vec_first(&res.errors) } as *const *const core::ffi::c_char;
        let mut errors = Vec::with_capacity(count.max(0) as usize);
        for i in 0..count.max(0) as usize {
            // SAFETY: i < count; each entry is a NUL-terminated string or null.
            let e = unsafe { *first.add(i) };
            if !e.is_null() {
                errors.push(
                    unsafe { CStr::from_ptr(e) }
                        .to_string_lossy()
                        .as_ref()
                        .to_owned(),
                );
            }
        }

        // SAFETY: frees what ecs_shell_run allocated in `res`; nothing above keeps a pointer into it.
        unsafe { sys::ecs_shell_result_fini(world, &mut res) };
        CommandResult { value, errors }
    }
}
