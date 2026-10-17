//! Turning an `HWND` into the thing a person would name.
//!
//! Three problems, in order of how much they distort a day's totals:
//!
//! 1. UWP apps all report `ApplicationFrameHost.exe`. Left alone, Calculator, Settings and
//!    the Store merge into one row.
//! 2. The executable path is stable but unreadable. The version resource holds the name the
//!    vendor chose, which is what belongs in the GUI.
//! 3. Titles are read across a process boundary and must not be able to hang us.

use std::collections::HashMap;

use crate::platform::{Detected, Window};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, MAX_PATH};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

/// The host process every UWP window claims to belong to.
const UWP_HOST: &str = "applicationframehost.exe";

/// Describe the foreground window, or `None` if it has no process we can name.
///
/// A window with an empty title is still returned: some apps genuinely have none, and
/// dropping them would silently lose the time.
pub fn describe(hwnd: HWND) -> Option<Window> {
    if hwnd.is_invalid() {
        return None;
    }
    let mut pid = 0u32;
    // Returns the thread id; zero means the window is already gone.
    if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) } == 0 || pid == 0 {
        return None;
    }

    let mut exe_path = exe_path_of(pid)?;
    if toki_core::ignore::exe_name(&exe_path) == UWP_HOST {
        if let Some((child_pid, child_exe)) = uwp_child(hwnd, pid) {
            pid = child_pid;
            exe_path = child_exe;
        }
    }

    Some(Window {
        pid,
        exe_path: toki_core::app::canonical_exe_path(&exe_path),
        title: title_of(hwnd),
        class: class_of(hwnd),
    })
}

/// The window caption.
///
/// `GetWindowTextW` is documented not to send `WM_GETTEXT` across a process boundary, so a
/// hung foreground app cannot take the hook thread down with it. That guarantee is the
/// reason this is safe to call from the pump.
pub fn title_of(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let written = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if written <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..written as usize])
}

/// The window's class name — registered with the window manager at creation, so unlike
/// `title_of` this never touches the target process and cannot hang on one.
///
/// Windows caps class names at 256 characters; a fixed buffer avoids the length-then-fetch
/// dance `title_of` needs for an unbounded caption.
pub fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let written = unsafe { GetClassNameW(hwnd, &mut buf) };
    if written <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..written as usize])
}

/// Full executable path of a process id.
///
/// `PROCESS_QUERY_LIMITED_INFORMATION` is the weakest right that answers this, and is the
/// reason an unelevated daemon can still name most protected processes. A few — anti-cheat
/// drivers, some system services — refuse anyway, and those become `None` rather than a
/// guess.
pub fn exe_path_of(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let guard = Handle(handle);

    let mut buf = vec![0u16; MAX_PATH as usize * 2];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            guard.0,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    if ok.is_err() || len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// The real process behind an `ApplicationFrameHost` window.
///
/// The frame is a shell-owned chrome around a child window belonging to the actual app, so
/// the app is found by walking children for one whose process differs from the host's.
fn uwp_child(frame: HWND, host_pid: u32) -> Option<(u32, String)> {
    struct Search {
        host_pid: u32,
        found: Option<(u32, String)>,
    }

    unsafe extern "system" fn visit(child: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(child, Some(&mut pid)) };
        if pid != 0 && pid != search.host_pid {
            if let Some(path) = exe_path_of(pid) {
                search.found = Some((pid, path));
                return false.into(); // stop enumerating
            }
        }
        true.into()
    }

    let mut search = Search {
        host_pid,
        found: None,
    };
    let _ = unsafe {
        EnumChildWindows(
            Some(frame),
            Some(visit),
            LPARAM(&mut search as *mut Search as isize),
        )
    };
    search.found
}

/// What Windows can say about an app: its name. The icon is the executable's own, which
/// the GUI reads itself.
pub fn detect(window: &Window) -> Detected {
    Detected {
        name: display_name(window),
        icon: None,
    }
}

/// `FileDescription` from an executable's version resource — "Google Chrome" for
/// `chrome.exe`.
///
/// Absent for plenty of programs, in which case the caller falls back to the file name.
/// Read once per app and cached in the database, because it touches the disk.
fn display_name(window: &Window) -> Option<String> {
    // Asked of the process rather than taken from `exe_path`, which has had any version
    // folder taken out and so may not be a file at all.
    let path = wide(&exe_path_of(window.pid).unwrap_or_else(|| window.exe_path.clone()));
    let size = unsafe { GetFileVersionInfoSizeW(PWSTR(path.as_ptr() as *mut u16), None) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    unsafe {
        GetFileVersionInfoW(
            PWSTR(path.as_ptr() as *mut u16),
            Some(0),
            size,
            data.as_mut_ptr() as *mut _,
        )
    }
    .ok()?;

    // The resource is keyed by language and codepage. Ask the file which it has rather than
    // assuming US English, which is how this fails on localised builds.
    let (lang, codepage) = translation(&data)?;
    let key = wide(&format!(
        "\\StringFileInfo\\{lang:04x}{codepage:04x}\\FileDescription"
    ));

    let mut value: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    let ok = unsafe {
        VerQueryValueW(
            data.as_ptr() as *const _,
            windows::core::PCWSTR(key.as_ptr()),
            &mut value,
            &mut len,
        )
    };
    if !ok.as_bool() || value.is_null() || len == 0 {
        return None;
    }
    // `len` counts characters including the terminator.
    let chars = unsafe { std::slice::from_raw_parts(value as *const u16, len as usize) };
    let name = String::from_utf16_lossy(chars)
        .trim_end_matches('\0')
        .trim()
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// First language/codepage pair the file declares.
fn translation(data: &[u8]) -> Option<(u16, u16)> {
    let key = wide("\\VarFileInfo\\Translation");
    let mut value: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    let ok = unsafe {
        VerQueryValueW(
            data.as_ptr() as *const _,
            windows::core::PCWSTR(key.as_ptr()),
            &mut value,
            &mut len,
        )
    };
    if !ok.as_bool() || value.is_null() || len < 4 {
        return None;
    }
    let pair = unsafe { std::slice::from_raw_parts(value as *const u16, 2) };
    Some((pair[0], pair[1]))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Closes on drop, so an early return cannot leak a process handle.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Remembers what a window belongs to, because a title change does not move it.
///
/// A video's caption updates as it plays and an address bar updates as you type, so the
/// same `HWND` is asked about hundreds of times a minute. Resolving it every time would
/// mean an `OpenProcess` per keystroke.
#[derive(Default)]
pub struct WindowCache {
    /// pid, exe path, window class — the class is cached alongside the exe path rather than
    /// re-read like the title, because it is fixed at window creation and never changes.
    by_hwnd: HashMap<isize, (u32, String, String)>,
}

impl WindowCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Describe a window, reusing the cached process unless the handle has been recycled.
    ///
    /// Handles are recycled, so the pid is re-read every time — that call is a lookup in
    /// the window manager, not a process open — and only a mismatch forces the expensive
    /// path.
    pub fn describe(&mut self, hwnd: HWND) -> Option<Window> {
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) } == 0 || pid == 0 {
            return None;
        }

        let key = hwnd.0 as isize;
        if let Some((cached_pid, exe_path, class)) = self.by_hwnd.get(&key) {
            // A UWP frame caches the child's pid, which never matches the frame's, so the
            // host pid is kept alongside it under the same key by `insert` below.
            if *cached_pid == pid {
                return Some(Window {
                    pid,
                    exe_path: exe_path.clone(),
                    title: title_of(hwnd),
                    class: class.clone(),
                });
            }
        }

        let window = describe(hwnd)?;
        // Key on the pid the window manager reports, not the unwrapped one, so the next
        // lookup for a UWP frame still hits.
        self.by_hwnd
            .insert(key, (pid, window.exe_path.clone(), window.class.clone()));
        Some(window)
    }

    pub fn forget(&mut self, hwnd: HWND) {
        self.by_hwnd.remove(&(hwnd.0 as isize));
    }

    /// Drop everything once the map has grown past what a desktop plausibly has open.
    ///
    /// Closed windows are never reported, so entries would otherwise accumulate for the
    /// life of the process. Clearing wholesale costs one re-resolve per live window and
    /// avoids carrying eviction policy for a map that holds a few hundred short strings.
    pub fn trim(&mut self, limit: usize) {
        if self.by_hwnd.len() > limit {
            self.by_hwnd.clear();
        }
    }
}
