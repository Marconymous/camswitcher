//! CamSwitch: pick several cameras, switch between them with global hotkeys,
//! and expose the result as one virtual camera ("CamSwitch Camera").

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod capture;
mod compositor;
mod config;
mod core;
mod debug_ui;
mod devices;
mod hotkeys;
mod stills;
mod sysinfo;
mod toast;
mod tray;
mod ui;
mod vcam;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use eframe::egui;
use parking_lot::Mutex;
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::compositor::Preview;
use crate::devices::DeviceList;

/// Enumerates devices on a COM MTA worker thread (the UI thread is STA).
pub fn scan_devices_async() -> Receiver<DeviceList> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        match devices::enumerate() {
            Ok(d) => {
                let _ = tx.send(d);
            }
            Err(e) => log::warn!("device enumeration failed: {e}"),
        }
        unsafe { CoUninitialize() };
    });
    rx
}

fn init_logging() {
    let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> = vec![simplelog::TermLogger::new(
        log::LevelFilter::Info,
        simplelog::Config::default(),
        simplelog::TerminalMode::Stderr,
        simplelog::ColorChoice::Auto,
    )];
    if let Some(dir) = config::Config::path().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(f) = std::fs::File::create(dir.join("camswitch.log")) {
            loggers.push(simplelog::WriteLogger::new(log::LevelFilter::Info, simplelog::Config::default(), f));
        }
    }
    let _ = simplelog::CombinedLogger::init(loggers);
}

fn main() -> eframe::Result {
    init_logging();
    if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
        log::error!("MFStartup failed: {e}");
    }

    let devices = scan_devices_async().recv().unwrap_or_default();
    let config = config::Config::load();
    let start_hidden = config.start_minimized || std::env::args().any(|a| a == autostart::MINIMIZED_ARG);
    let core = Arc::new(Mutex::new(core::Core::new(config, devices)));
    let preview = Arc::new(Mutex::new(Preview {
        rgba: Vec::new(),
        seq: 0,
        output_size: (shared::DEFAULT_WIDTH, shared::DEFAULT_HEIGHT),
        shm_connected: false,
        consumer_active: false,
        stats: Default::default(),
    }));
    let vcam = vcam::VirtualCamera::start();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("CamSwitch").with_inner_size([1100.0, 640.0])
            .with_visible(!start_hidden),
        ..Default::default()
    };
    eframe::run_native(
        "CamSwitch",
        options,
        Box::new(move |cc| {
            let stop = Arc::new(AtomicBool::new(false));
            let ui_heartbeat = Arc::new(AtomicU64::new(shared::shm::now_ms()));
            let ctx = cc.egui_ctx.clone();
            compositor::spawn(
                core.clone(),
                preview.clone(),
                ui_heartbeat.clone(),
                move || ctx.request_repaint(),
                stop.clone(),
            );

            let window = tray::WindowCtl { ctx: cc.egui_ctx.clone(), quitting: Arc::new(AtomicBool::new(false)) };
            let has_tray = match tray::create(core.clone(), window.clone()) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("tray icon unavailable: {e}");
                    if start_hidden {
                        window.show();
                    }
                    false
                }
            };
            toast::start();

            let after_hotkey = {
                let core = core.clone();
                let ctx = cc.egui_ctx.clone();
                move || {
                    let (view, text, show_toast) = {
                        let c = core.lock();
                        (tray::TrayView::of(&c), c.status_text(), c.config.show_toast)
                    };
                    tray::sync(&view);
                    if show_toast {
                        toast::show(&text);
                    }
                    ctx.request_repaint();
                }
            };
            let mut hotkeys = hotkeys::Hotkeys::new(core.clone(), after_hotkey).map_err(|e| e.to_string());
            if let Ok(h) = &mut hotkeys {
                h.apply(&core.lock().config.hotkeys);
            }

            Ok(Box::new(ui::App {
                core,
                preview,
                hotkeys,
                vcam,
                stop,
                ui_heartbeat,
                texture: None,
                last_seq: 0,
                recording: None,
                device_scan: None,
                last_scan: Instant::now(),
                show_debug: false,
                sysinfo: sysinfo::SysInfo::new(),
                window,
                has_tray,
                start_hidden: (start_hidden && has_tray).then_some(0),
                editing: None,
                transform_dirty: false,
            }))
        }),
    )
}
