//! The tray icon, as a StatusNotifierItem — what Waybar, KDE and most other bars host.
//!
//! The same menu as on Windows. A left click opens the GUI, matching what a Windows user
//! gets from a click on the icon.

use std::sync::OnceLock;

use chrono::Utc;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, OfflineReason, ToolTip, Tray};
use tracing::{info, warn};

use toki_core::event::Millis;

use super::pump;
use crate::platform::Raw;

const MINUTE_MS: Millis = 60 * 1_000;

static HANDLE: OnceLock<Handle<TokiTray>> = OnceLock::new();

struct TokiTray {
    paused: bool,
}

/// Put the icon in the tray, now or whenever a tray host appears.
///
/// The collector starts with the graphical session, which on Omarchy is before the shell
/// that hosts the tray: quickshell came up three minutes after the daemon on 2026-10-17. A
/// single attempt then failed with no watcher on the bus, and the icon never appeared,
/// though tracking ran all along. So a missing watcher is not an error: the service stays
/// on the bus and registers the moment one takes the name, and again each time the shell
/// restarts, as Omarchy's does on a theme change. Without any host ever, it waits quietly;
/// the tray is a convenience, not the collector.
pub fn spawn() {
    match (TokiTray { paused: false })
        .assume_sni_available(true)
        .spawn()
    {
        Ok(handle) => {
            let _ = HANDLE.set(handle);
        }
        Err(e) => warn!(%e, "no tray available"),
    }
}

/// Take the icon down before the process goes, so the bar does not keep a dead entry.
pub fn remove() {
    if let Some(h) = HANDLE.get() {
        h.shutdown().wait();
    }
}

/// Reflect the run loop's pause state in the menu and the tooltip.
pub fn set_paused(paused: bool) {
    if let Some(h) = HANDLE.get() {
        h.update(|t| t.paused = paused);
    }
}

/// Show the weekly digest as a desktop notification. Returns whether it was shown, so a
/// failure leaves the digest for the next try, as on Windows.
pub fn show_digest(title: &str, body: &str) -> bool {
    let notify = || -> zbus::Result<u32> {
        let conn = zbus::blocking::Connection::session()?;
        let hints = std::collections::HashMap::<&str, zbus::zvariant::Value>::new();
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "Toki",
                0u32,
                "",
                title,
                body,
                Vec::<&str>::new(),
                hints,
                -1i32,
            ),
        )?
        .body()
        .deserialize()
    };
    match notify() {
        Ok(_) => true,
        Err(e) => {
            warn!(%e, "could not show the weekly digest");
            false
        }
    }
}

fn now() -> Millis {
    Utc::now().timestamp_millis()
}

fn pause(minutes: Option<Millis>) {
    let at = now();
    pump::send(Raw::Pause {
        at,
        until: minutes.map(|m| at + m * MINUTE_MS),
    });
}

impl Tray for TokiTray {
    fn id(&self) -> String {
        "toki".into()
    }

    fn title(&self) -> String {
        "Toki".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        [22, 32, 48]
            .into_iter()
            .map(|size| {
                // `paint` gives BGRA; the protocol wants ARGB in network byte order.
                let data = crate::platform::icon::paint(size)
                    .chunks_exact(4)
                    .flat_map(|p| [p[3], p[2], p[1], p[0]])
                    .collect();
                Icon {
                    width: size as i32,
                    height: size as i32,
                    data,
                }
            })
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: if self.paused {
                "Toki — paused"
            } else {
                "Toki — tracking"
            }
            .into(),
            ..Default::default()
        }
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        match reason {
            OfflineReason::No => info!("tray host went away; the icon returns with it"),
            OfflineReason::Error(e) => info!(%e, "no tray host yet; the icon appears with one"),
            _ => {}
        }
        // Keep waiting: the host coming back is what puts the icon back.
        true
    }

    fn watcher_online(&self) {
        info!("tray host appeared; registering the icon");
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        pump::open_gui();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: &str, f: fn()| -> MenuItem<Self> {
            StandardItem {
                label: label.into(),
                activate: Box::new(move |_| f()),
                ..Default::default()
            }
            .into()
        };
        let mut menu = vec![
            StandardItem {
                label: "Open Toki".into(),
                // Greyed rather than hidden while the GUI is not installed beside us: the
                // entry is the only hint that there is one.
                enabled: crate::paths::gui_path().is_some(),
                activate: Box::new(|_| pump::open_gui()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        if self.paused {
            menu.push(item("Resume tracking", || {
                pump::send(Raw::Unpause { at: now() })
            }));
        } else {
            menu.push(item("Pause for 15 minutes", || pause(Some(15))));
            menu.push(item("Pause for 1 hour", || pause(Some(60))));
            menu.push(item("Pause until resumed", || pause(None)));
        }
        menu.push(MenuItem::Separator);
        menu.push(item("Quit Toki", || pump::send(Raw::Stop { at: now() })));
        menu
    }
}
