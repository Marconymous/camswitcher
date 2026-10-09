//! Types and helpers shared between the CamSwitch app and the virtual camera DLL.

pub mod image;
pub mod shm;
pub mod stats;

/// CLSID of the virtual camera media source (COM class in `vcam_source.dll`).
pub const VCAM_CLSID_STR: &str = "{A30A3BBE-F718-4438-97C6-DAC726888072}";
pub const VCAM_CLSID_U128: u128 = 0xA30A3BBE_F718_4438_97C6_DAC726888072;

/// Friendly name of the virtual camera as shown in other apps.
pub const VCAM_FRIENDLY_NAME: &str = "CamSwitch Camera";

pub const DEFAULT_WIDTH: u32 = 1280;
pub const DEFAULT_HEIGHT: u32 = 720;
pub const FPS: u32 = 30;

/// Output resolutions advertised by the virtual camera.
pub const OUTPUT_SIZES: [(u32, u32); 2] = [(1280, 720), (1920, 1080)];
