//! COM self-registration under HKLM (requires an elevated `regsvr32`).

use windows::Win32::Foundation::{HMODULE, WIN32_ERROR};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteTreeW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

fn clsid_key() -> String {
    format!(r"SOFTWARE\Classes\CLSID\{}", shared::VCAM_CLSID_STR)
}

fn check(err: WIN32_ERROR) -> Result<()> {
    err.ok()
}

fn wide_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
}

fn set_string(key: HKEY, name: PCWSTR, value: &str) -> Result<()> {
    check(unsafe { RegSetValueExW(key, name, None, REG_SZ, Some(&wide_bytes(value))) })
}

fn create_key(path: &str) -> Result<HKEY> {
    let mut key = HKEY::default();
    check(unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    })?;
    Ok(key)
}

pub fn register() -> Result<()> {
    let mut buf = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(HMODULE(crate::module_handle())), &mut buf) } as usize;
    if len == 0 {
        return Err(windows::core::Error::from_thread());
    }
    let path = String::from_utf16_lossy(&buf[..len]);

    let clsid = create_key(&clsid_key())?;
    let res = set_string(clsid, PCWSTR::null(), shared::VCAM_FRIENDLY_NAME);
    unsafe {
        let _ = RegCloseKey(clsid);
    }
    res?;

    let inproc = create_key(&format!(r"{}\InprocServer32", clsid_key()))?;
    let res = set_string(inproc, PCWSTR::null(), &path).and_then(|_| set_string(inproc, w!("ThreadingModel"), "Both"));
    unsafe {
        let _ = RegCloseKey(inproc);
    }
    res
}

pub fn unregister() -> Result<()> {
    check(unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(clsid_key())) })
}
