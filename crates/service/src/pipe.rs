//! Named pipe creation with explicit DACL.
//!
//! Tokio's `ServerOptions` calls `CreateNamedPipeW` with `NULL` security
//! attributes, which yields the Windows default DACL — for a LocalSystem
//! process this grants Administrators + LocalSystem only. That's correct
//! for the admin control pipe (`PIPE_NAME_ADMIN`), but the status pipe
//! (`PIPE_NAME_STATUS`) is meant for unprivileged callers like the tray UI,
//! and the default DACL refuses them at the OS layer.
//!
//! This module wraps the raw Win32 calls so the status pipe can be created
//! with an explicit, auditable DACL via SDDL. The handle is then transferred
//! into tokio's async pipe server via `from_raw_handle`, so the rest of the
//! IPC code path is unchanged.
//!
//! The admin pipe is created via the existing tokio API (`ServerOptions`)
//! and intentionally keeps the default DACL.

#![cfg(windows)]

use anyhow::{anyhow, Context, Result};
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use tracing::debug;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, HLOCAL, INVALID_HANDLE_VALUE};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::PSECURITY_DESCRIPTOR;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

/// SDDL granting unprivileged status access plus admin/system everything.
///
/// * `D:` — DACL
/// * `(A;;GA;;;BA)` — Allow Generic All to Built-in Administrators
/// * `(A;;GA;;;SY)` — Allow Generic All to LocalSystem
/// * `(A;;GA;;;AU)` — Allow Generic All to Authenticated Users
///
/// Authenticated Users get full pipe access so they can round-trip a
/// request (write the request, read the response). The service enforces
/// "read-only requests only" on the status pipe in the IPC handler — the
/// ACL is the outer boundary, the per-request check inside `serve_ipc`
/// is the inner boundary and the load-bearing one.
const STATUS_PIPE_SDDL: &str = "D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;GA;;;AU)";

/// Pipe buffer sizes. Status traffic is small (status snapshot + event
/// stream); 64 KB is plenty without wasting kernel non-paged pool.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// Pipe wait timeout (ms) for `WaitNamedPipe`. Default == 50 ms is fine for
/// the synchronous open path; tokio uses overlapped I/O so this is mostly
/// a fallback for non-tokio clients.
const PIPE_DEFAULT_TIMEOUT_MS: u32 = 50;

/// Create a server-side named pipe with the status DACL. Returns a tokio
/// async pipe server with the same semantics as `ServerOptions::create`,
/// just with our custom security descriptor.
///
/// `first_instance` should be `true` for the very first pipe instance the
/// service binds (to defeat squatting via `FILE_FLAG_FIRST_PIPE_INSTANCE`)
/// and `false` for subsequent instances in the accept loop.
pub fn create_status_pipe(name: &str, first_instance: bool) -> Result<NamedPipeServer> {
    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let sddl_wide: Vec<u16> = STATUS_PIPE_SDDL
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // Convert SDDL → SECURITY_DESCRIPTOR. The descriptor is allocated by
    // the API and must be released with LocalFree once we're done with it
    // (CreateNamedPipeW copies the DACL into the kernel object).
    let mut psd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: sddl_wide is a valid null-terminated UTF-16 string for the
    // lifetime of this call. `&mut psd` is a valid out-parameter pointer.
    // We do NOT use the size out-parameter (None).
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl_wide.as_ptr()),
            SDDL_REVISION_1,
            &mut psd,
            None,
        )
    }
    .context("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;

    // Wrap descriptor lifetime: we MUST LocalFree on every exit path below.
    let _sd_guard = SdGuard(psd);

    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: false.into(),
    };

    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first_instance {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT;

    // SAFETY: name_wide is null-terminated; sa is constructed above and
    // valid for the duration of the call; psd is alive (held by _sd_guard).
    // CreateNamedPipeW returns INVALID_HANDLE_VALUE on failure — we check
    // manually since this signature returns a raw HANDLE, not a Result.
    let raw_handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name_wide.as_ptr()),
            open_mode,
            pipe_mode,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            PIPE_DEFAULT_TIMEOUT_MS,
            Some(&sa),
        )
    };

    if raw_handle == INVALID_HANDLE_VALUE {
        // SAFETY: we always read last_os_error after a Win32 call that
        // signalled failure via a sentinel return.
        let err = std::io::Error::last_os_error();
        return Err(anyhow!("CreateNamedPipeW({name}) failed: {err}"));
    }

    debug!(pipe = %name, "status pipe created with permissive DACL");

    // Transfer ownership into a tokio async pipe server. tokio requires the
    // pipe to have been created with FILE_FLAG_OVERLAPPED (which we did).
    //
    // SAFETY: `raw_handle` is a valid, kernel-allocated pipe handle that we
    // own exclusively. `from_raw_handle` documents that the caller must
    // ensure the handle is a server-side named pipe opened with
    // FILE_FLAG_OVERLAPPED — both invariants hold.
    let server = unsafe { NamedPipeServer::from_raw_handle(raw_handle.0 as *mut _) }
        .context("NamedPipeServer::from_raw_handle")?;
    Ok(server)
}

/// Create the admin pipe with the default Windows DACL (Administrators +
/// LocalSystem only). Just delegates to tokio's `ServerOptions` — we keep
/// the helper here so the call sites in `runtime.rs` are symmetric with
/// `create_status_pipe`.
pub fn create_admin_pipe(name: &str, first_instance: bool) -> Result<NamedPipeServer> {
    ServerOptions::new()
        .pipe_mode(PipeMode::Byte)
        .first_pipe_instance(first_instance)
        // tokio caps `max_instances` at 254 (`PIPE_UNLIMITED_INSTANCES - 1`
        // — the raw Win32 sentinel is 255 but tokio reserves one slot, and
        // passing 255 panics with "cannot specify more than 254 instances").
        // The tray's foreground reporter fires every 250 ms on the admin
        // pipe; without this lift any concurrent client (CLI status query,
        // a second tray, etc.) would hit ERROR_PIPE_BUSY for the brief
        // window between accept-and-spawn and the next listener
        // instantiation. 254 effectively means "unlimited" for our access
        // pattern.
        .max_instances(254)
        .create(name)
        .with_context(|| format!("create admin pipe {name}"))
}

/// RAII wrapper that releases the SECURITY_DESCRIPTOR allocated by
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW` on drop. The
/// kernel copies the DACL into the pipe object during `CreateNamedPipeW`,
/// so we can free our copy immediately after that call returns — but
/// keeping it alive via this guard makes the cleanup obvious even on
/// the error paths.
struct SdGuard(PSECURITY_DESCRIPTOR);

impl Drop for SdGuard {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: psd was allocated by ConvertString...Descriptor...W,
            // which documents LocalFree as the corresponding release call.
            // We never double-free — Drop runs at most once.
            unsafe {
                let _ = LocalFree(HLOCAL(self.0 .0));
            }
        }
    }
}
