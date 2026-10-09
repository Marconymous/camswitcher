//! Registers the virtual camera with Windows for the lifetime of the app.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use parking_lot::Mutex;
use windows::Win32::Foundation::REGDB_E_CLASSNOTREG;
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, MFCreateVirtualCamera, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::HSTRING;

#[derive(Clone, Debug)]
pub enum VcamState {
    Starting,
    Running,
    Failed(String),
}

pub struct VirtualCamera {
    pub state: Arc<Mutex<VcamState>>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl VirtualCamera {
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(VcamState::Starting));
        let (tx, rx) = mpsc::channel::<()>();
        let thread = {
            let state = state.clone();
            std::thread::Builder::new()
                .name("vcam".into())
                .spawn(move || unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    let result = MFCreateVirtualCamera(
                        MFVirtualCameraType_SoftwareCameraSource,
                        MFVirtualCameraLifetime_Session,
                        MFVirtualCameraAccess_CurrentUser,
                        &HSTRING::from(shared::VCAM_FRIENDLY_NAME),
                        &HSTRING::from(shared::VCAM_CLSID_STR),
                        None,
                    )
                    .and_then(|cam| cam.Start(None::<&IMFAsyncCallback>).map(|_| cam));
                    match result {
                        Ok(cam) => {
                            log::info!("virtual camera started");
                            *state.lock() = VcamState::Running;
                            let _ = rx.recv();
                            let _ = cam.Remove();
                            let _ = cam.Shutdown();
                            log::info!("virtual camera removed");
                        }
                        Err(e) => {
                            log::error!("virtual camera failed: {e}");
                            let msg = if e.code() == REGDB_E_CLASSNOTREG {
                                "Virtual camera DLL not registered. Run scripts\\register.ps1 as administrator.".to_string()
                            } else {
                                format!("{} ({:#010x})", e.message(), e.code().0)
                            };
                            *state.lock() = VcamState::Failed(msg);
                        }
                    }
                    CoUninitialize();
                })
                .expect("spawn vcam thread")
        };
        Self { state, stop: Some(tx), thread: Some(thread) }
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
