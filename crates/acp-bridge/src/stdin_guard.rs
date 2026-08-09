//! Make the JSON-RPC wire un-inheritable, so a forgotten `stdin(null)` cannot deadlock a child.
//!
//! `liberado_common::process::command` nulls each child's stdin, and
//! `crates/test-support/tests/subprocess_rules.rs` fails the build if a call site skips it.
//! That covers code we own. It does not cover a dependency that spawns a process itself, and it
//! did not exist on the afternoon a Paseo prompt hung for 19 minutes because every `git`
//! inherited this process's stdin and blocked reading protocol traffic meant for the agent.
//!
//! This is the belt to that pair of braces. [`take_wire_stdin`] hands back a **private**
//! duplicate of the real stdin for the JSON-RPC reader, then points the *process-level*
//! `STD_INPUT_HANDLE` at the null device. Children inherit the null device whatever they do, and
//! the wire is reachable only through the handle this function returned.
//!
//! Order matters and is easy to get backwards: the duplicate must be taken **before** the swap,
//! and the read loop must use that duplicate rather than `tokio::io::stdin()` — which resolves
//! `STD_INPUT_HANDLE` on every read and would otherwise see EOF from the null device and exit
//! the bridge immediately.
//!
//! Non-Windows is a no-op. Inheriting fd 0 is the same hazard there, but the fix belongs with
//! `pre_exec` on the spawn side, and the helper already covers every site we own.

/// A private handle to the ACP wire, detached from the process-level stdin.
///
/// `None` means the swap did not happen and the caller should keep reading normal stdin — the
/// helper is still in force, so that is the status quo rather than a broken bridge.
pub type WireStdin = Option<std::fs::File>;

#[cfg(windows)]
pub fn take_wire_stdin() -> WireStdin {
    use std::fs::{File, OpenOptions};
    use std::os::windows::io::{FromRawHandle, RawHandle};

    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;

    unsafe extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> *mut std::ffi::c_void;
        fn SetStdHandle(n_std_handle: u32, h_handle: *mut std::ffi::c_void) -> i32;
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn DuplicateHandle(
            h_source_process: *mut std::ffi::c_void,
            h_source: *mut std::ffi::c_void,
            h_target_process: *mut std::ffi::c_void,
            lp_target: *mut *mut std::ffi::c_void,
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_options: u32,
        ) -> i32;
    }

    // SAFETY: plain Win32 handle calls; every pointer is either a pseudo-handle from
    // GetCurrentProcess or an out-param we own.
    let dup = unsafe {
        let current = GetStdHandle(STD_INPUT_HANDLE);
        if current.is_null() {
            return None;
        }
        let mut dup: *mut std::ffi::c_void = std::ptr::null_mut();
        // `b_inherit_handle = 0`: the duplicate is explicitly NOT inheritable, so even this
        // private copy cannot reach a child by accident.
        let ok = DuplicateHandle(
            GetCurrentProcess(),
            current,
            GetCurrentProcess(),
            &mut dup,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        );
        if ok == 0 || dup.is_null() {
            return None;
        }
        dup
    };

    // Only now is it safe to take the wire off the process handle.
    let Ok(nul) = OpenOptions::new().read(true).open("NUL") else {
        // Could not open the null device: keep the duplicate rather than leaving the process
        // with no usable stdin at all.
        // SAFETY: `dup` came from DuplicateHandle and is owned by us.
        return Some(unsafe { File::from_raw_handle(dup as RawHandle) });
    };
    // SAFETY: `nul` is a live handle for the lifetime of the process (leaked below).
    let swapped = unsafe {
        use std::os::windows::io::AsRawHandle;
        SetStdHandle(STD_INPUT_HANDLE, nul.as_raw_handle().cast()) != 0
    };
    // Deliberately leaked: this handle *is* the process stdin now. Dropping the `File` would
    // close it and leave STD_INPUT_HANDLE dangling — worse than the bug being prevented.
    std::mem::forget(nul);

    if !swapped {
        tracing::warn!("could not detach stdin from children; relying on per-spawn nulling");
    }
    // SAFETY: `dup` came from DuplicateHandle and is owned by us from here on.
    Some(unsafe { File::from_raw_handle(dup as RawHandle) })
}

#[cfg(not(windows))]
pub fn take_wire_stdin() -> WireStdin {
    None
}
