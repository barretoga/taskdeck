//! Windows default device mute via Core Audio (used when Voicemeeter is not in use).
use windows::core::Result;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eCapture, eConsole, eRender, EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

#[derive(Clone, Copy)]
pub enum Flow {
    Speakers,
    Mic,
}

impl Flow {
    fn data_flow(self) -> EDataFlow {
        match self {
            Flow::Speakers => eRender,
            Flow::Mic => eCapture,
        }
    }
}

pub struct Audio {
    enumerator: IMMDeviceEnumerator,
}

impl Audio {
    pub fn new() -> Result<Self> {
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        Ok(Self { enumerator })
    }

    /// The default device can change at any time, so it is looked up on every call
    /// (a cheap in-process COM call).
    fn endpoint(&self, flow: Flow) -> Result<IAudioEndpointVolume> {
        unsafe {
            let dev = self.enumerator.GetDefaultAudioEndpoint(flow.data_flow(), eConsole)?;
            dev.Activate(CLSCTX_ALL, None)
        }
    }

    pub fn is_muted(&self, flow: Flow) -> Option<bool> {
        unsafe { self.endpoint(flow).and_then(|v| v.GetMute()).ok().map(|b| b.as_bool()) }
    }

    pub fn set_muted(&self, flow: Flow, muted: bool) -> bool {
        unsafe {
            self.endpoint(flow)
                .and_then(|v| v.SetMute(BOOL::from(muted), std::ptr::null()))
                .is_ok()
        }
    }
}
