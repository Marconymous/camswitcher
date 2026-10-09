//! Video capture device enumeration via Media Foundation.

use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    MFCreateAttributes, MFEnumDeviceSources,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{GUID, PWSTR, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub link: String,
}

pub fn get_string(attrs: &IMFAttributes, key: &GUID) -> Result<String> {
    unsafe {
        let mut p = PWSTR::null();
        let mut len = 0;
        attrs.GetAllocatedString(key, &mut p, &mut len)?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        Ok(s)
    }
}

/// Result of a device scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceList {
    /// Cameras usable as inputs.
    pub cameras: Vec<DeviceInfo>,
    /// Our own virtual camera(s): shown in the UI, never selectable.
    pub own: Vec<DeviceInfo>,
}

/// Lists video capture devices, splitting off our own virtual camera.
pub fn enumerate() -> Result<DeviceList> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;

        let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut arr, &mut count)?;
        let mut out = DeviceList::default();
        if !arr.is_null() {
            for slot in std::slice::from_raw_parts_mut(arr, count as usize) {
                if let Some(act) = slot.take() {
                    let name = get_string(&act, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
                    let link = get_string(&act, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)
                        .unwrap_or_default();
                    // Windows lists ours as "CamSwitch Camera (Windows Virtual Camera)".
                    if link.is_empty() {
                        continue;
                    }
                    if name.starts_with(shared::VCAM_FRIENDLY_NAME) {
                        out.own.push(DeviceInfo { name, link });
                    } else {
                        out.cameras.push(DeviceInfo { name, link });
                    }
                }
            }
            CoTaskMemFree(Some(arr as _));
        }
        Ok(out)
    }
}
