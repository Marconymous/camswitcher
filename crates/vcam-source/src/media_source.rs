//! The media source Frame Server talks to. One live video stream.

use std::sync::Mutex;

use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, ComObject, GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};

use crate::media_stream::{MediaStream, STREAM_ID, ks_error};

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl, IMFSampleAllocatorControl)]
pub struct MediaSource {
    queue: IMFMediaEventQueue,
    attributes: IMFAttributes,
    descriptor: IMFPresentationDescriptor,
    stream: ComObject<MediaStream>,
    shutdown: Mutex<bool>,
}

impl MediaSource {
    pub fn create(activate_attrs: &IMFAttributes) -> Result<IMFMediaSource> {
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            let attributes = attributes.unwrap();
            activate_attrs.CopyAllItems(&attributes)?;

            let stream = MediaStream::create()?;
            let descriptor = MFCreatePresentationDescriptor(Some(&[Some(stream.descriptor())]))?;
            descriptor.SelectStream(0)?;

            let source: IMFMediaSourceEx = MediaSource {
                queue: MFCreateEventQueue()?,
                attributes,
                descriptor,
                stream: stream.clone(),
                shutdown: Mutex::new(false),
            }
            .into();
            let source: IMFMediaSource = source.cast()?;
            stream.set_source(source.clone());
            Ok(source)
        }
    }

    fn check_shutdown(&self) -> Result<()> {
        if *self.shutdown.lock().unwrap() { Err(MF_E_SHUTDOWN.into()) } else { Ok(()) }
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.queue.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        self.check_shutdown()?;
        unsafe { self.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(&self, met: u32, ext: *const GUID, hr: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        self.check_shutdown()?;
        unsafe { self.queue.QueueEventParamVar(met, ext, hr, value) }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.check_shutdown()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        self.check_shutdown()?;
        unsafe { self.descriptor.Clone() }
    }

    fn Start(
        &self,
        pd: Ref<IMFPresentationDescriptor>,
        time_format: *const GUID,
        _start: *const PROPVARIANT,
    ) -> Result<()> {
        self.check_shutdown()?;
        let pd = pd.ok()?;
        if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
            return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
        }
        unsafe {
            let time = MFGetSystemTime();
            for i in 0..pd.GetStreamDescriptorCount()? {
                let mut selected = BOOL(0);
                let mut desc = None;
                pd.GetStreamDescriptorByIndex(i, &mut selected, &mut desc)?;
                let desc = desc.unwrap();
                if desc.GetStreamIdentifier()? != STREAM_ID {
                    return Err(MF_E_INVALIDSTREAMNUMBER.into());
                }
                if selected.as_bool() {
                    let media_type = desc.GetMediaTypeHandler()?.GetCurrentMediaType()?;
                    let event = if self.stream.is_running() { MEUpdatedStream } else { MENewStream };
                    let unk: IUnknown = self.stream.to_interface();
                    self.queue.QueueEventParamUnk(event.0 as u32, &GUID::zeroed(), S_OK, &unk)?;
                    self.stream.start(&media_type, time)?;
                } else {
                    self.stream.stop()?;
                }
            }
            self.queue.QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::from(time))
        }
    }

    fn Stop(&self) -> Result<()> {
        self.check_shutdown()?;
        self.stream.stop()?;
        unsafe { self.queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null()) }
    }

    fn Pause(&self) -> Result<()> {
        self.check_shutdown()?;
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        {
            let mut shutdown = self.shutdown.lock().unwrap();
            if *shutdown {
                return Ok(());
            }
            *shutdown = true;
        }
        log::info!("media source shutdown");
        self.stream.shutdown();
        unsafe { self.queue.Shutdown() }
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        self.check_shutdown()?;
        Ok(self.attributes.clone())
    }

    fn GetStreamAttributes(&self, id: u32) -> Result<IMFAttributes> {
        self.check_shutdown()?;
        if id != STREAM_ID {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        Ok(self.stream.attributes())
    }

    fn SetD3DManager(&self, _manager: Ref<IUnknown>) -> Result<()> {
        // CPU-memory samples only; Frame Server copies them where needed.
        self.check_shutdown()
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(&self, _service: *const GUID, _riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> Result<()> {
        if !ppv.is_null() {
            unsafe { *ppv = std::ptr::null_mut() };
        }
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(ks_error())
    }
    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(ks_error())
    }
    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(ks_error())
    }
}

impl IMFSampleAllocatorControl_Impl for MediaSource_Impl {
    fn SetDefaultAllocator(&self, stream_id: u32, allocator: Ref<IUnknown>) -> Result<()> {
        self.check_shutdown()?;
        if stream_id != STREAM_ID {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        let allocator = match allocator.as_ref() {
            Some(a) => Some(a.cast::<IMFVideoSampleAllocatorEx>()?),
            None => None,
        };
        log::info!("SetDefaultAllocator (present: {})", allocator.is_some());
        self.stream.set_allocator(allocator);
        Ok(())
    }

    fn GetAllocatorUsage(&self, stream_id: u32, input_id: *mut u32, usage: *mut MFSampleAllocatorUsage) -> Result<()> {
        self.check_shutdown()?;
        if stream_id != STREAM_ID {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        unsafe {
            *input_id = stream_id;
            *usage = MFSampleAllocatorUsage_UsesProvidedAllocator;
        }
        Ok(())
    }
}
