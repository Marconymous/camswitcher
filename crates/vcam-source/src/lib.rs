//! CamSwitch virtual camera: a Media Foundation custom media source exposed as
//! an in-proc COM server. Windows Frame Server loads it after the app calls
//! `MFCreateVirtualCamera` with [`CLSID_VCAM`].
//!
//! Object graph: class factory -> [`activate::Activator`] (IMFActivate) ->
//! [`media_source::MediaSource`] -> one [`media_stream::MediaStream`].

#![allow(non_snake_case)]

mod activate;
mod frames;
mod media_source;
mod media_stream;
mod registry;

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, HINSTANCE, S_FALSE,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface, Ref, implement};

pub const CLSID_VCAM: GUID = GUID::from_u128(shared::VCAM_CLSID_U128);

static MODULE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

pub(crate) fn module_handle() -> *mut c_void {
    MODULE.load(Ordering::Relaxed)
}

/// Logs to `C:\ProgramData\CamSwitch\vcam.log` (best effort).
pub(crate) fn init_logging() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let dir = std::path::Path::new(r"C:\ProgramData\CamSwitch");
        let _ = std::fs::create_dir_all(dir);
        // Different host processes/accounts may not share write access to one file.
        let open = |name: String| std::fs::OpenOptions::new().create(true).append(true).open(dir.join(name));
        let file = open("vcam.log".into()).or_else(|_| open(format!("vcam-{}.log", std::process::id())));
        if let Ok(file) = file {
            let _ = simplelog::WriteLogger::init(log::LevelFilter::Info, simplelog::Config::default(), file);
        }
        log::info!("vcam-source loaded in pid {}", std::process::id());
    });
}

#[unsafe(no_mangle)]
extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(hinst.0, Ordering::Relaxed);
    }
    true.into()
}

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if ppv.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activator: IUnknown = activate::Activator::create()?;
        unsafe { activator.query(riid, ppv).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = std::ptr::null_mut() };
    if unsafe { *rclsid } != CLSID_VCAM {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    init_logging();
    let factory: IClassFactory = ClassFactory.into();
    unsafe { factory.query(riid, ppv) }
}

/// Keep the DLL loaded for the lifetime of the hosting process; Frame Server
/// recycles its host process anyway, and unloading under live objects is worse.
#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match registry::register() {
        Ok(()) => HRESULT(0),
        Err(e) => e.code(),
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    match registry::unregister() {
        Ok(()) => HRESULT(0),
        Err(e) => e.code(),
    }
}
