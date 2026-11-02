//! The COM/WinRT layer of `cmux-toast` (Windows-only calls behind the
//! crate's pure API). Windows 0.62.2 API, symbol-verified against the
//! registry sources.
//!
//! Contract with [`super`]: the worker thread calls `initialize_apartment`
//! once (MTA), then `shortcut_matches` / `write_shortcut` / `post_toast`
//! freely — all assume an initialized apartment.
//!
//! Booking the AUMID (the Windows contract for unpackaged apps): `.lnk`
//! anywhere under the Start Menu registering `System.AppUserModel.ID` in
//! its shell item property store. Writing is two-phase: build the link
//! (IShellLinkW → IPersistFile::Save), then open the *saved file's*
//! property store (SHGetPropertyStoreFromParsingName, GPS_READWRITE) and
//! set the property there.

use std::path::{Path, PathBuf};

use windows::core::{Interface, Result as WinResult, HSTRING, PWSTR};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::{VT_EMPTY, VT_LPWSTR};
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, IShellLinkW,
    PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreFromParsingName, GPS_DEFAULT, GPS_READWRITE,
    },
    SHGetKnownFolderPath, ShellLink, KNOWN_FOLDER_FLAG,
};
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

use super::ToastError;

/// `System.AppUserModel.ID` — the exact key Windows checks on the
/// shortcut's property store. Defined locally instead of pulling the
/// generator's `EnhancedStorage` feature for one constant.
const PKEY_APP_USER_MODEL_ID: windows::Win32::Foundation::PROPERTYKEY =
    windows::Win32::Foundation::PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };

/// Registers this thread as COM MTA (worker-thread init; never on the UI
/// thread). `S_FALSE` (already initialized) still counts as success.
pub fn initialize_apartment() -> Result<(), ToastError> {
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() {
        Err(ToastError::Windows(format!("CoInitializeEx: {hr}")))
    } else {
        Ok(())
    }
}

/// The per-user Start Menu Programs folder.
pub fn known_programs_dir() -> Result<PathBuf, ToastError> {
    let raw = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_Programs,
            KNOWN_FOLDER_FLAG::default(),
            None, // no token: the current user's folders
        )
    }
    .map_err(|e| ToastError::Windows(format!("SHGetKnownFolderPath: {e}")))?;
    // PWSTR owns UTF-16; decode wide, then release the shell allocation.
    let path = unsafe { raw.to_string() }
        .map_err(|e| ToastError::Windows(format!("folder path decode: {e}")))?;
    unsafe { CoTaskMemFree(Some(raw.as_ptr() as *const std::ffi::c_void)) };
    Ok(PathBuf::from(path))
}

/// Null-terminated UTF-16 for PCWSTR fields.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Reads the AUMID property from an existing shortcut; `None` when absent.
fn read_shortcut_aumid(lnk: &Path) -> Result<Option<String>, ToastError> {
    let path = HSTRING::from(lnk.as_os_str().to_string_lossy().to_string());
    let store = unsafe {
        SHGetPropertyStoreFromParsingName::<_, _, IPropertyStore>(
            &path,
            None::<&windows::Win32::System::Com::IBindCtx>,
            GPS_DEFAULT,
        )
    }
    .map_err(|e| ToastError::Windows(format!("open {lnk:?}: {e}")))?;
    let pv = unsafe {
        store
            .GetValue(&PKEY_APP_USER_MODEL_ID)
            .map_err(|e| ToastError::Windows(format!("read AUMID property: {e}")))?
    };
    let out = match unsafe { pv.Anonymous.Anonymous.vt } {
        VT_LPWSTR => {
            let raw = unsafe { pv.Anonymous.Anonymous.Anonymous.pwszVal.0 as *const u16 };
            decode_wide_ptr(raw)
        }
        _ => None,
    };
    // Never leak the variant's allocation when it owned one.
    let mut to_clear = pv;
    unsafe { PropVariantClear(&mut to_clear).ok() };
    Ok(out)
}

fn decode_wide_ptr(raw: *const u16) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    let mut len = 0usize;
    while unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    let u16s = unsafe { std::slice::from_raw_parts(raw, len) };
    Some(String::from_utf16_lossy(u16s))
}

/// `true` when the link exists and already registers `aumid`.
pub fn shortcut_matches(lnk: &Path, aumid: &str) -> Result<bool, ToastError> {
    if !lnk.exists() {
        return Ok(false);
    }
    let registered = read_shortcut_aumid(lnk)?;
    Ok(registered.as_deref() == Some(aumid))
}

/// Builds the per-user link and stamps its AUMID property.
pub fn write_shortcut(
    target_exe: &Path,
    lnk: &Path,
    aumid: &str,
    description: &str,
) -> Result<(), ToastError> {
    write_shortcut_inner(target_exe, lnk, aumid, description)
        .map_err(|e| ToastError::Windows(format!("registering {lnk:?}: {e}")))
}

fn write_shortcut_inner(
    target_exe: &Path,
    lnk: &Path,
    aumid: &str,
    description: &str,
) -> WinResult<()> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let exe_w = wide(&target_exe.as_os_str().to_string_lossy());
        link.SetPath(windows::core::PCWSTR(exe_w.as_ptr()))?;
        link.SetIconLocation(windows::core::PCWSTR(exe_w.as_ptr()), 0)?;
        let description_w = wide(description);
        link.SetDescription(windows::core::PCWSTR(description_w.as_ptr()))?;
        let persist: IPersistFile = link.cast()?;
        let lnk_w = wide(&lnk.as_os_str().to_string_lossy());
        persist.Save(windows::core::PCWSTR(lnk_w.as_ptr()), true)?;

        // Second phase: stamp the AUMID into the saved file's store (the
        // link object itself has no store for it).
        let path_h = HSTRING::from(lnk.as_os_str().to_string_lossy().to_string());
        let store: IPropertyStore = SHGetPropertyStoreFromParsingName::<_, _, IPropertyStore>(
            &path_h,
            None::<&windows::Win32::System::Com::IBindCtx>,
            GPS_READWRITE,
        )?;
        let aumid_w = wide(aumid);
        // The store copies on SetValue; the buffer must survive the call.
        let mut aumid_buf: Vec<u16> = aumid_w.clone();
        let mut prop = PROPVARIANT::default();
        // PROPVARIANT_0's string member hides behind ManuallyDrop: take the
        // place explicitly before writing the union (borrow ends before
        // the store sees `prop`).
        {
            let slot = &mut *prop.Anonymous.Anonymous;
            slot.vt = VT_LPWSTR;
            slot.Anonymous.pwszVal = PWSTR(aumid_buf.as_mut_ptr());
        }
        store.SetValue(&PKEY_APP_USER_MODEL_ID, &prop)?;
        store.Commit()?;
        // Hand-build the variant back to empty instead of PropVariantClear:
        // that helper frees VT_LPWSTR pointers with CoTaskMemFree, but this
        // buffer is Vec-backed — freeing it on the Windows allocator corrupts
        // the heap (observed crash 0xC0000374). Dropping the Vec below frees
        // it on the allocator that owns it.
        {
            let slot = &mut *prop.Anonymous.Anonymous;
            slot.vt = VT_EMPTY;
            slot.Anonymous.pwszVal = PWSTR::default();
        }
        drop(prop);
        drop(aumid_buf);
        drop(aumid_w);
    }
    Ok(())
}

/// Posts one already-built toast XML under an AUMID.
pub fn post_toast(aumid: &str, xml: &str) -> Result<(), ToastError> {
    post_toast_inner(aumid, xml).map_err(|e| ToastError::Windows(format!("toast post: {e}")))
}

fn post_toast_inner(aumid: &str, xml: &str) -> WinResult<()> {
    let document = windows::Data::Xml::Dom::XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))?;
    notifier.Show(&toast)?;
    Ok(())
}
