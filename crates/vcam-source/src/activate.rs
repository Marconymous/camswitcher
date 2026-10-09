//! `IMFActivate` returned by the class factory. Frame Server sets attributes on
//! it, then calls `ActivateObject` to get the media source.

use std::ffi::c_void;
use std::sync::Mutex;

use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFAttributes_Impl, IMFMediaSource, MF_ATTRIBUTE_TYPE,
    MF_ATTRIBUTES_MATCH_TYPE, MFCreateAttributes,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, GUID, IUnknown, Interface, PCWSTR, PWSTR, Ref, Result, implement};

#[implement(IMFActivate)]
pub struct Activator {
    attrs: IMFAttributes,
    source: Mutex<Option<IMFMediaSource>>,
}

impl Activator {
    pub fn create() -> Result<IUnknown> {
        let mut attrs = None;
        unsafe { MFCreateAttributes(&mut attrs, 4)? };
        let activator: IMFActivate = Activator { attrs: attrs.unwrap(), source: Mutex::new(None) }.into();
        Ok(activator.cast()?)
    }
}

impl IMFActivate_Impl for Activator_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        let mut guard = self.source.lock().unwrap();
        let source = match guard.as_ref() {
            Some(s) => s.clone(),
            None => {
                log::info!("ActivateObject: creating media source");
                let s = crate::media_source::MediaSource::create(&self.attrs).inspect_err(|e| {
                    log::error!("media source creation failed: {e}");
                })?;
                *guard = Some(s.clone());
                s
            }
        };
        unsafe { source.query(riid, ppv).ok() }
    }

    fn ShutdownObject(&self) -> Result<()> {
        if let Some(source) = self.source.lock().unwrap().take() {
            unsafe {
                let _ = source.Shutdown();
            }
        }
        Ok(())
    }

    fn DetachObject(&self) -> Result<()> {
        self.source.lock().unwrap().take();
        Ok(())
    }
}

// IMFAttributes: delegate everything to the inner attribute store.
impl IMFAttributes_Impl for Activator_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.GetItem(key, (!value.is_null()).then_some(value)) }
    }
    fn GetItemType(&self, key: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.attrs.GetItemType(key) }
    }
    fn CompareItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<BOOL> {
        unsafe { self.attrs.CompareItem(key, value) }
    }
    fn Compare(&self, theirs: Ref<IMFAttributes>, matchtype: MF_ATTRIBUTES_MATCH_TYPE) -> Result<BOOL> {
        unsafe { self.attrs.Compare(theirs.as_ref(), matchtype) }
    }
    fn GetUINT32(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetUINT32(key) }
    }
    fn GetUINT64(&self, key: *const GUID) -> Result<u64> {
        unsafe { self.attrs.GetUINT64(key) }
    }
    fn GetDouble(&self, key: *const GUID) -> Result<f64> {
        unsafe { self.attrs.GetDouble(key) }
    }
    fn GetGUID(&self, key: *const GUID) -> Result<GUID> {
        unsafe { self.attrs.GetGUID(key) }
    }
    fn GetStringLength(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetStringLength(key) }
    }
    fn GetString(&self, key: *const GUID, value: PWSTR, size: u32, len: *mut u32) -> Result<()> {
        unsafe { (Interface::vtable(&self.attrs).GetString)(self.attrs.as_raw(), key, value, size, len).ok() }
    }
    fn GetAllocatedString(&self, key: *const GUID, value: *mut PWSTR, len: *mut u32) -> Result<()> {
        unsafe { self.attrs.GetAllocatedString(key, value, len) }
    }
    fn GetBlobSize(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetBlobSize(key) }
    }
    fn GetBlob(&self, key: *const GUID, buf: *mut u8, size: u32, len: *mut u32) -> Result<()> {
        unsafe { (Interface::vtable(&self.attrs).GetBlob)(self.attrs.as_raw(), key, buf, size, len).ok() }
    }
    fn GetAllocatedBlob(&self, key: *const GUID, buf: *mut *mut u8, size: *mut u32) -> Result<()> {
        unsafe { self.attrs.GetAllocatedBlob(key, buf, size) }
    }
    fn GetUnknown(&self, key: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        unsafe { (Interface::vtable(&self.attrs).GetUnknown)(self.attrs.as_raw(), key, riid, ppv).ok() }
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.SetItem(key, value) }
    }
    fn DeleteItem(&self, key: *const GUID) -> Result<()> {
        unsafe { self.attrs.DeleteItem(key) }
    }
    fn DeleteAllItems(&self) -> Result<()> {
        unsafe { self.attrs.DeleteAllItems() }
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> Result<()> {
        unsafe { self.attrs.SetUINT32(key, value) }
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> Result<()> {
        unsafe { self.attrs.SetUINT64(key, value) }
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> Result<()> {
        unsafe { self.attrs.SetDouble(key, value) }
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> Result<()> {
        unsafe { self.attrs.SetGUID(key, value) }
    }
    fn SetString(&self, key: *const GUID, value: &PCWSTR) -> Result<()> {
        unsafe { self.attrs.SetString(key, *value) }
    }
    fn SetBlob(&self, key: *const GUID, buf: *const u8, size: u32) -> Result<()> {
        let slice = if buf.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(buf, size as usize) } };
        unsafe { self.attrs.SetBlob(key, slice) }
    }
    fn SetUnknown(&self, key: *const GUID, value: Ref<IUnknown>) -> Result<()> {
        unsafe { self.attrs.SetUnknown(key, value.as_ref()) }
    }
    fn LockStore(&self) -> Result<()> {
        unsafe { self.attrs.LockStore() }
    }
    fn UnlockStore(&self) -> Result<()> {
        unsafe { self.attrs.UnlockStore() }
    }
    fn GetCount(&self) -> Result<u32> {
        unsafe { self.attrs.GetCount() }
    }
    fn GetItemByIndex(&self, index: u32, key: *mut GUID, value: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.GetItemByIndex(index, key, (!value.is_null()).then_some(value)) }
    }
    fn CopyAllItems(&self, dest: Ref<IMFAttributes>) -> Result<()> {
        unsafe { self.attrs.CopyAllItems(dest.as_ref()) }
    }
}
