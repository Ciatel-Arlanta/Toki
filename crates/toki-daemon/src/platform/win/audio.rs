//! Whether the app you are looking at is playing sound.
//!
//! This is the exemption that stops a two-hour film from being logged as five minutes. It
//! is asked only once input silence has crossed the idle threshold, then on each tick for
//! as long as sound holds the session open. An ordinary active stretch never pays for it.
//!
//! The comparison is by executable path, not process id. Chromium renders audio from a
//! separate utility process, so a tab playing a video has a different pid from the window
//! in front of you — but the same `chrome.exe`. Matching on pid would make the exemption
//! fail for exactly the case it exists to serve.

use windows::core::Interface;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use super::window::exe_path_of;
use crate::platform::Window;

/// COM apartment for the calling thread, torn down on drop.
///
/// Held rather than initialised per probe because `CoInitializeEx`/`CoUninitialize` around
/// every call would drop and rebuild the audio client each time.
pub struct Com {
    /// False when the thread was already initialised by someone else, in which case
    /// uninitialising on drop would be someone else's problem.
    owned: bool,
}

impl Com {
    pub fn init() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self { owned: hr.is_ok() }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

/// Is any audio session belonging to the window's executable currently rendering?
///
/// Every failure answers "no". A missing audio device, a machine with sound disabled, or a
/// COM error should end an idle session at the last input, which is the behaviour without
/// the exemption at all.
pub fn is_rendering(window: &Window) -> bool {
    render_check(&window.exe_path).unwrap_or(false)
}

fn render_check(exe_path: &str) -> windows::core::Result<bool> {
    let target = toki_core::ignore::exe_name(exe_path);

    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
    // The default multimedia render endpoint is where films and music go. Communications
    // is the call device and is deliberately not consulted.
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) }?;
    let manager: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None) }?;
    let sessions = unsafe { manager.GetSessionEnumerator() }?;

    let count = unsafe { sessions.GetCount() }?;
    for i in 0..count {
        let Ok(control) = (unsafe { sessions.GetSession(i) }) else {
            continue;
        };
        let Ok(control) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };
        // Expired and inactive sessions linger in the enumerator long after the sound
        // stopped, so the state has to be checked as well as the owner.
        if unsafe { control.GetState() } != Ok(AudioSessionStateActive) {
            continue;
        }
        let Ok(pid) = (unsafe { control.GetProcessId() }) else {
            continue;
        };
        // The system session reports pid 0 and belongs to no application.
        if pid == 0 {
            continue;
        }
        if let Some(path) = exe_path_of(pid) {
            if toki_core::ignore::exe_name(&path) == target {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
