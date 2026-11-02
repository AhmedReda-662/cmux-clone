//! The per-user pipe security descriptor (ADR-0005 `cmuxOnly` default):
//! a protected DACL granting file-all access to the current user's SID
//! only — SDDL `D:P(A;;FA;;;<SID>)`. No cross-user client passes and the
//! protected flag keeps admins from silently widening it.
//!
//! The SID comes from the process token (`OpenProcessToken` +
//! `GetTokenInformation(TokenUser)`, then `ConvertSidToStringSidW`), and
//! the descriptor from `ConvertStringSecurityDescriptorToSecurityDescriptorW`
//! — documented APIs instead of raw struct math.

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The current user's SID string (`S-1-5-21-…`); the returned string is
/// fully owned (the PWSTR allocation is LocalFree'd after decode).
pub fn current_user_sid() -> Result<String> {
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let decode = || -> Result<String> {
            // TOKEN_USER is variable-sized: query with an empty buffer to
            // get the needed size, then fetch into it.
            let mut needed = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
            if needed == 0 {
                return Err(windows::core::Error::from_hresult(
                    GetLastError().to_hresult(),
                ));
            }
            let mut buffer = vec![0u8; needed as usize];
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )?;
            let user = *(buffer.as_ptr() as *const TOKEN_USER);
            let mut string = windows::core::PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut string)?;
            let sid = string.to_string()?;
            let _ = LocalFree(Some(HLOCAL(string.0.cast())));
            Ok(sid)
        };
        let result = decode();
        let _ = CloseHandle(token);
        result
    }
}

/// SDDL for the pipe DACL: file-all for the current user, protected.
pub fn pipe_sddl_for_current_user() -> Result<String> {
    Ok(format!("D:P(A;;FA;;;{})", current_user_sid()?))
}

/// Materializes an SDDL string into a `PSECURITY_DESCRIPTOR` (self-relative)
/// for `SECURITY_ATTRIBUTES`. The caller frees it with `local_free_sddl` when
/// done with the pipe handle.
pub fn sddl_descriptor(sddl: &str) -> Result<PSECURITY_DESCRIPTOR> {
    let wide = windows::core::HSTRING::from(sddl);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR::from_raw(wide.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )?;
    }
    Ok(descriptor)
}

/// Frees a descriptor from [`sddl_descriptor`].
pub fn local_free_sddl(descriptor: PSECURITY_DESCRIPTOR) {
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0.cast())));
    }
}
