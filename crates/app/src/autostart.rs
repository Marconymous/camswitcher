//! "Start with Windows": a `CamSwitch` value under the current user's `Run`
//! key. The registry is the source of truth, nothing is kept in the config.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE: PCWSTR = w!("CamSwitch");
/// Command-line flag: start hidden in the tray.
pub const MINIMIZED_ARG: &str = "--minimized";

pub fn is_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set(on: bool) -> Result<(), String> {
    unsafe {
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let cmd = format!("\"{}\" {MINIMIZED_ARG}", exe.display());
            let data: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                REG_SZ.0,
                Some(data.as_ptr().cast()),
                (data.len() * 2) as u32,
            )
            .ok()
            .map_err(|e| e.to_string())
        } else {
            match RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) {
                e if e == ERROR_FILE_NOT_FOUND => Ok(()),
                e => e.ok().map_err(|e| e.to_string()),
            }
        }
    }
}
