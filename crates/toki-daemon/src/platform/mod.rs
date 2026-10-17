//! The line between deciding and observing.
//!
//! No module under here decides what gets recorded. They report facts — which window is in
//! front, when the keyboard was last touched, whether sound is playing, whether the machine
//! went to sleep — and [`toki_core`] draws the conclusions. That line is what makes the rules
//! testable without a desktop, and what lets one collector run on two operating systems.
//!
//! Each OS gets a module with the same shape: `audio`, `autostart`, `console`, `idle`, `ipc`,
//! `pump`, `single`, `tray` and `window`, with the same public functions. The run loop only
//! ever names them through this module, so it cannot tell which one it got.

use toki_core::event::Millis;

mod icon;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

/// What the pump saw. Interpreting it is the run loop's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Raw {
    /// A different window came forward, or the foreground window's caption changed.
    Focus {
        at: Millis,
        window: Window,
    },
    Suspend {
        at: Millis,
    },
    /// Woken, or unlocked. The run loop responds by re-reading the foreground window,
    /// because Windows does not reliably re-announce it.
    Resume {
        at: Millis,
    },
    Lock {
        at: Millis,
    },
    /// Stop recording. `until` is a duration in milliseconds; `None` means until the user
    /// says otherwise. Sent by the tray, never by the OS.
    Pause {
        at: Millis,
        until: Option<Millis>,
    },
    Unpause {
        at: Millis,
    },
    /// A new idle threshold from Settings, to take effect immediately rather than waiting
    /// for the next restart. Sent by IPC, never by the OS or the tray.
    SetConfig {
        idle_threshold_ms: Millis,
    },
    /// Logging off or shutting down. The last chance to close cleanly.
    Stop {
        at: Millis,
    },
}

/// What the OS says about an app beyond its path, read once per app per run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detected {
    /// The name a person knows it by.
    pub name: Option<String>,
    /// What the GUI should draw it with, where that is not the executable itself: the
    /// `Icon=` of its `.desktop` entry on Linux. Always `None` on Windows, where the GUI
    /// reads the icon out of the executable.
    pub icon: Option<String>,
}

/// Everything the collector needs about the window that just came forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub pid: u32,
    pub exe_path: String,
    pub title: String,
    /// A fixed, un-localized identifier for the kind of window, unlike the caption: the Win32
    /// window class on Windows, the Wayland app id (or X11 `WM_CLASS`) on Linux. Exists to
    /// tell apart windows that share an executable: explorer.exe is the desktop, the
    /// taskbar, the Alt-Tab switcher, the tray overflow flyout, and a real File Explorer
    /// window, all at once.
    pub class: String,
}
