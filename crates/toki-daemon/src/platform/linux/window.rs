//! Turning a Hyprland client into the thing a person would name.
//!
//! The Windows problems have Linux twins:
//!
//! 1. Shared runtimes. Every system-Electron app on Arch reports `/usr/lib/electron*/electron`,
//!    every Omarchy web app reports Chromium, and an AppImage reports a fresh `/tmp/.mount_*`
//!    path each launch. For those the window's app id is the stable identity, not the exe.
//! 2. No version resource. The name a person knows is in the app's `.desktop` file.

use std::collections::HashSet;
use std::path::PathBuf;

use toki_core::ignore::exe_name;

use super::hyprland::Client;
use crate::platform::{Detected, Window};

/// Describe a Hyprland client, or `None` if its process is already gone.
pub fn describe(client: &Client) -> Option<Window> {
    let pid = u32::try_from(client.pid).ok()?;
    let exe = exe_path_of(pid)?;
    Some(Window {
        pid,
        exe_path: app_key(&exe, &client.class),
        title: client.title.clone(),
        class: client.class.clone(),
    })
}

/// The executable behind a process.
///
/// A binary replaced by a package upgrade while running reads back with ` (deleted)` on the
/// end, which would make one app into two the day it was updated.
pub fn exe_path_of(pid: u32) -> Option<String> {
    let path = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let path = path.to_string_lossy();
    Some(path.strip_suffix(" (deleted)").unwrap_or(&path).to_string())
}

/// What a window's time is recorded against: its executable, unless that is shared with
/// unrelated apps, in which case its app id.
fn app_key(exe: &str, class: &str) -> String {
    if class.is_empty() || !(is_shared_runtime(exe) || web_app_host(class).is_some()) {
        exe.to_string()
    } else {
        class.to_string()
    }
}

/// Executables that host many different apps.
fn is_shared_runtime(exe: &str) -> bool {
    let name = exe_name(exe);
    exe.starts_with("/tmp/.mount_")
        || name.starts_with("electron")
        || name.starts_with("python")
        || matches!(name.as_str(), "java" | "node" | "gjs" | "bwrap")
}

/// The site behind a Chromium `--app` window, whose app id Chromium spells
/// `chrome-<host>__<path>-<profile>`. This is how every Omarchy web app is launched.
fn web_app_host(class: &str) -> Option<&str> {
    let rest = class.strip_prefix("chrome-")?;
    let host = rest.split("__").next()?;
    (host.contains('.') && !host.contains('/')).then_some(host)
}

/// The app's name and icon, from its `.desktop` file, found by — in order of how sure the
/// match is — file name equal to the app id, `StartupWMClass`, the web app's URL, or the
/// executable. Failing all of those, the name is the executable's file name and there is
/// no icon.
///
/// Read once per app per run and cached in the database, because it reads a directory of
/// small files.
pub fn detect(window: &Window) -> Detected {
    let class = window.class.to_ascii_lowercase();
    let exe = exe_path_of(window.pid).unwrap_or_else(|| window.exe_path.clone());
    let program = exe_name(&exe);
    let host = web_app_host(&window.class);
    let url = host.map(|h| format!("://{h}"));

    let rank = |e: &DesktopEntry| -> Option<Rank> {
        if !class.is_empty() && e.id.eq_ignore_ascii_case(&class) {
            return Some(Rank::Id);
        }
        if !class.is_empty() && e.wm_class.eq_ignore_ascii_case(&class) {
            return Some(Rank::WmClass);
        }
        if url.as_ref().is_some_and(|u| e.exec.contains(u.as_str())) {
            return Some(Rank::WebApp);
        }
        let first = e.exec.split_whitespace().next().unwrap_or("");
        (first == exe || exe_name(first.trim_matches('"')) == program).then_some(Rank::Exe)
    };
    let dirs = application_dirs();
    let found = entry_named(&dirs, &window.class).or_else(|| best_entry(&dirs, rank));

    Detected {
        icon: found
            .as_ref()
            .map(|e| e.icon.clone())
            .filter(|i| !i.is_empty()),
        name: found
            .map(|e| e.name)
            .or_else(|| host.map(str::to_string))
            // Plenty of Linux programs ship no `.desktop` file. Their file name is still
            // kinder than the full path the GUI would otherwise show.
            .or_else(|| {
                let file = window.exe_path.rsplit('/').next()?;
                (!file.is_empty()).then(|| file.to_string())
            }),
    }
}

/// How sure a `.desktop` entry is to be a window's app, surest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Id,
    WmClass,
    WebApp,
    Exe,
}

#[derive(Debug, Default)]
struct DesktopEntry {
    /// File name without `.desktop`.
    id: String,
    name: String,
    exec: String,
    wm_class: String,
    /// A theme icon name, or an absolute path to an image.
    icon: String,
}

/// Where `.desktop` files live, user ones first so they shadow the system's — the same
/// order launchers use.
fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => dirs.push(PathBuf::from(d)),
        None => dirs.extend(home.map(|h| h.join(".local/share"))),
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').map(PathBuf::from));
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// The entry whose file is named exactly for `app_id`, found without listing anything.
///
/// The freedesktop convention, and what a well-behaved Wayland app does, is an app id equal
/// to its desktop file id, so most windows end here after a few `stat`s and one read. The
/// first file found is the one that shadows the rest, as in [`best_entry`]. One that cannot
/// be used — no `Name=` — sends the caller to the full scan, which treats it the same way.
fn entry_named(dirs: &[PathBuf], app_id: &str) -> Option<DesktopEntry> {
    if app_id.is_empty() || app_id.contains('/') {
        return None;
    }
    let file = format!("{app_id}.desktop");
    let path = dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())?;
    parse_entry(app_id, &std::fs::read_to_string(path).ok()?)
}

/// The best-ranked application entry, the first of equals in shadowing order, reading one
/// file at a time and keeping only the best so far, rather than every entry on the system.
/// Stops early on a match no other entry could beat. Hidden and `NoDisplay` entries still
/// count: they are often exactly the ones that name a helper window.
fn best_entry(
    dirs: &[PathBuf],
    rank: impl Fn(&DesktopEntry) -> Option<Rank>,
) -> Option<DesktopEntry> {
    let mut seen = HashSet::new();
    let mut best: Option<(Rank, DesktopEntry)> = None;
    for dir in dirs {
        let Ok(files) = std::fs::read_dir(dir) else {
            continue;
        };
        for path in files.flatten().map(|f| f.path()) {
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".desktop"))
            else {
                continue;
            };
            if !seen.insert(id.to_string()) {
                continue;
            }
            let Some(entry) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse_entry(id, &text))
            else {
                continue;
            };
            let Some(r) = rank(&entry) else {
                continue;
            };
            if best.as_ref().map_or(true, |(b, _)| r < *b) {
                best = Some((r, entry));
                if r == Rank::Id {
                    return best.map(|(_, e)| e);
                }
            }
        }
    }
    best.map(|(_, e)| e)
}

/// The `[Desktop Entry]` group's unlocalised keys. Actions and other groups come after it
/// and carry their own `Name=`, which must not overwrite the app's.
fn parse_entry(id: &str, text: &str) -> Option<DesktopEntry> {
    let mut entry = DesktopEntry {
        id: id.to_string(),
        ..Default::default()
    };
    let mut in_main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim().to_string();
            match key.trim() {
                "Name" => entry.name = value,
                "Exec" => entry.exec = value,
                "StartupWMClass" => entry.wm_class = value,
                "Icon" => entry.icon = value,
                _ => {}
            }
        }
    }
    (!entry.name.is_empty()).then_some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_app_is_its_executable() {
        assert_eq!(
            app_key("/opt/zen-browser-bin/zen-bin", "zen"),
            "/opt/zen-browser-bin/zen-bin"
        );
    }

    #[test]
    fn apps_sharing_a_runtime_are_told_apart_by_app_id() {
        assert_eq!(app_key("/usr/lib/electron37/electron", "code"), "code");
        assert_eq!(
            app_key("/tmp/.mount_Obsidabc/obsidian", "obsidian"),
            "obsidian"
        );
        // Without an app id there is nothing better than the executable.
        assert_eq!(
            app_key("/usr/lib/electron37/electron", ""),
            "/usr/lib/electron37/electron"
        );
    }

    #[test]
    fn web_apps_are_their_own_app_but_the_browser_is_not() {
        let class = "chrome-web.whatsapp.com__-Default";
        assert_eq!(web_app_host(class), Some("web.whatsapp.com"));
        assert_eq!(app_key("/usr/lib/chromium/chromium", class), class);
        assert_eq!(web_app_host("chromium"), None);
        assert_eq!(
            app_key("/usr/lib/chromium/chromium", "chromium"),
            "/usr/lib/chromium/chromium"
        );
    }

    #[test]
    fn only_the_main_group_names_the_app() {
        let text = "[Desktop Entry]\nName=Zen Browser\nName[de]=Zen\nExec=/opt/zen-browser-bin/zen-bin %u\nStartupWMClass=zen\nIcon=zen-browser\n\n[Desktop Action new-window]\nName=New Window\nIcon=window-new\n";
        let e = parse_entry("zen", text).unwrap();
        assert_eq!(e.name, "Zen Browser");
        assert_eq!(e.icon, "zen-browser");
        assert_eq!(e.wm_class, "zen");
        assert!(e.exec.starts_with("/opt/zen-browser-bin/zen-bin"));
    }

    /// Two `applications` directories, user then system, each holding `(file id, text)`.
    fn dirs(user: &[(&str, &str)], system: &[(&str, &str)]) -> (tempdir::Dir, Vec<PathBuf>) {
        let root = tempdir::Dir::new();
        let mut out = Vec::new();
        for (name, files) in [("user", user), ("system", system)] {
            let dir = root.0.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            for (id, text) in files {
                std::fs::write(dir.join(format!("{id}.desktop")), text).unwrap();
            }
            out.push(dir);
        }
        (root, out)
    }

    /// A scratch directory removed on drop; the crate has no dev-dependency for one.
    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                let path = std::env::temp_dir().join(format!(
                    "toki-desktop-test-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                Self(path)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn entry(name: &str, exec: &str, extra: &str) -> String {
        format!("[Desktop Entry]\nName={name}\nExec={exec}\n{extra}")
    }

    #[test]
    fn the_surest_match_wins_whatever_order_the_files_come_in() {
        let by_exe = entry("By Exe", "/usr/bin/app", "Icon=exe-icon");
        let by_class = entry(
            "By Class",
            "launcher",
            "StartupWMClass=App\nIcon=class-icon",
        );
        let (_tmp, dirs) = dirs(&[("a-by-exe", &by_exe)], &[("z-by-class", &by_class)]);

        let found = best_entry(&dirs, |e| match e.id.as_str() {
            "a-by-exe" => Some(Rank::Exe),
            "z-by-class" => Some(Rank::WmClass),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            (found.name.as_str(), found.icon.as_str()),
            ("By Class", "class-icon")
        );
    }

    #[test]
    fn a_user_entry_shadows_the_system_one_of_the_same_id() {
        let user = entry("Mine", "foot", "Icon=my-foot");
        let system = entry("Foot", "foot", "Icon=foot");
        let (_tmp, dirs) = dirs(&[("foot", &user)], &[("foot", &system)]);

        assert_eq!(entry_named(&dirs, "foot").unwrap().icon, "my-foot");
        let scanned = best_entry(&dirs, |_| Some(Rank::Exe)).unwrap();
        assert_eq!(scanned.icon, "my-foot");
    }

    #[test]
    fn an_app_id_names_its_entry_without_a_scan() {
        let t3 = entry(
            "T3 Code (Nightly)",
            "\"/usr/lib/t3code-nightly/t3code\" %U",
            "Icon=/home/u/.local/share/icons/com.t3tools.T3Code.desktop.png",
        );
        let (_tmp, dirs) = dirs(&[("com.t3tools.T3Code", &t3)], &[]);

        let e = entry_named(&dirs, "com.t3tools.T3Code").unwrap();
        assert_eq!(e.name, "T3 Code (Nightly)");
        assert!(e.icon.starts_with('/'));
        assert!(entry_named(&dirs, "").is_none());
        assert!(entry_named(&dirs, "../com.t3tools.T3Code").is_none());
        assert!(entry_named(&dirs, "org.example.Missing").is_none());
    }
}
