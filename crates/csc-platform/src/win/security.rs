//! Security descriptor for the core's named pipe.
//!
//! A pipe created by an elevated or SYSTEM core would by default only let
//! administrators and SYSTEM write to it, so the tray (a normal user
//! process) could not talk to it. This descriptor lets the signed-in
//! (interactive) user read and write, without letting it create pipe
//! instances of its own, and labels the pipe medium integrity.

use windows::core::HSTRING;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

/// SYSTEM, administrators and the owner: full access. Interactive users:
/// FILE_GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | FILE_WRITE_EA
/// (no FILE_APPEND_DATA, which for a pipe means "create instance").
/// Mandatory label medium, no-write-up.
pub const PIPE_SDDL: &str =
    "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x12019b;;;IU)S:(ML;;NW;;;ME)";

pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attrs: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    /// Pointer for `ServerOptions::create_with_security_attributes_raw`.
    pub fn as_ptr(&mut self) -> *mut core::ffi::c_void {
        self.attrs.lpSecurityDescriptor = self.descriptor.0;
        (&mut self.attrs as *mut SECURITY_ATTRIBUTES).cast()
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(HLOCAL(self.descriptor.0));
        }
    }
}

/// The descriptor above when this process is elevated; `None` (use the
/// default) for a normal user process or when building it fails.
pub fn user_pipe_security() -> Option<PipeSecurity> {
    if !super::elevation::is_elevated() {
        return None;
    }
    let sddl = HSTRING::from(PIPE_SDDL);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        if let Err(e) = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &sddl,
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        ) {
            tracing::warn!("pipe security descriptor: {e}");
            return None;
        }
    }
    Some(PipeSecurity {
        descriptor,
        attrs: SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        },
    })
}
