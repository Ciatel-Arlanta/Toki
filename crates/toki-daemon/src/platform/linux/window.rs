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
    let target = Target::of_window(window);
    let dirs = application_dirs();
    let found = entry_named(&dirs, &target.app_id)
        .or_else(|| best_entries(&dirs, &[&target]).pop().flatten());

    Detected {
        icon: found
            .as_ref()
            .map(|e| e.icon.clone())
            .filter(|i| !i.is_empty()),
        name: found
            .map(|e| e.name)
            .or_else(|| web_app_host(&window.class).map(str::to_string))
            // Plenty of Linux programs ship no `.desktop` file. Their file name is still
            // kinder than the full path the GUI would otherwise show.
            .or_else(|| {
                let file = window.exe_path.rsplit('/').next()?;
                (!file.is_empty()).then(|| file.to_string())
            }),
    }
}

/// Icons for apps already in the database without one, as `(exe_path, icon)`: every app
/// last seen before icons were recorded, which otherwise keeps its lettered tile until it
/// next comes forward, however long that is. Each is `(exe_path, detected name)`.
///
/// There is no window to ask, so each is matched on what was stored: an app id where the
/// key is one (shared runtimes, web apps), the executable where it is a path, and failing
/// both, the entry whose `Name=` is the name detected from it before. All of them in one
/// pass over the entries, after the app ids that name their file directly.
pub fn recorded_icons(apps: &[(String, Option<String>)]) -> Vec<(String, String)> {
    icons_in(&application_dirs(), apps)
}

fn icons_in(dirs: &[PathBuf], apps: &[(String, Option<String>)]) -> Vec<(String, String)> {
    let targets: Vec<Target> = apps
        .iter()
        .map(|(key, name)| Target::of_record(key, name.as_deref()))
        .collect();
    let mut found: Vec<Option<DesktopEntry>> = targets
        .iter()
        .map(|t| entry_named(dirs, &t.app_id))
        .collect();
    let rest: Vec<usize> = (0..targets.len()).filter(|&i| found[i].is_none()).collect();
    if !rest.is_empty() {
        let asked: Vec<&Target> = rest.iter().map(|&i| &targets[i]).collect();
        for (i, entry) in rest.into_iter().zip(best_entries(dirs, &asked)) {
            found[i] = entry;
        }
    }
    apps.iter()
        .zip(found)
        .filter_map(|((key, _), e)| {
            let icon = e?.icon;
            (!icon.is_empty()).then(|| (key.clone(), icon))
        })
        .collect()
}

/// How sure a `.desktop` entry is to be a window's app, surest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Id,
    WmClass,
    WebApp,
    /// `Exec=` runs exactly this executable.
    ExePath,
    /// `Exec=` runs one of the same file name, such as a launcher on `PATH`.
    ExeName,
    /// Only for an app matched from its record: the entry its name was read from.
    Name,
}

/// What a `.desktop` entry is matched against: a live window, or an app as recorded.
struct Target {
    /// The Wayland app id as given, for finding a file named after it. Empty if unknown.
    app_id: String,
    /// The same, lowercased, for comparing.
    class: String,
    /// The executable, and its file name. Empty when only the app id is known.
    exe: String,
    program: String,
    /// `://<host>` of a Chromium `--app` window.
    url: Option<String>,
    /// The name detected for a recorded app, from the entry it should match again.
    name: Option<String>,
}

impl Target {
    fn of_window(window: &Window) -> Self {
        let exe = exe_path_of(window.pid).unwrap_or_else(|| window.exe_path.clone());
        Self {
            app_id: window.class.clone(),
            class: window.class.to_ascii_lowercase(),
            program: exe_name(&exe),
            exe,
            url: web_app_host(&window.class).map(|h| format!("://{h}")),
            name: None,
        }
    }

    /// An app by its key, which is an executable path, or an app id when the executable
    /// was a shared runtime (see `app_key`).
    fn of_record(key: &str, name: Option<&str>) -> Self {
        let name = name.filter(|n| !n.is_empty()).map(str::to_string);
        if key.contains('/') {
            return Self {
                app_id: String::new(),
                class: String::new(),
                exe: key.to_string(),
                program: exe_name(key),
                url: None,
                name,
            };
        }
        Self {
            app_id: key.to_string(),
            class: key.to_ascii_lowercase(),
            exe: String::new(),
            program: String::new(),
            url: web_app_host(key).map(|h| format!("://{h}")),
            name,
        }
    }

    /// Whether a file-name match can be this app. A window always can. A recorded app
    /// whose name came from an entry needs this one to carry that name: two installs of one
    /// program share a file name, and `/usr/lib/t3code/t3code`, whose entry was gone, must
    /// not take `/usr/lib/t3code-nightly/t3code`'s icon. A name that is only the file name
    /// says no entry was ever found, so it rules nothing out.
    fn same_app_by_name(&self, e: &DesktopEntry) -> bool {
        match &self.name {
            None => true,
            Some(n) => *n == e.name || *n == self.program,
        }
    }

    fn rank(&self, e: &DesktopEntry) -> Option<Rank> {
        if !self.class.is_empty() && e.id.eq_ignore_ascii_case(&self.class) {
            return Some(Rank::Id);
        }
        if !self.class.is_empty() && e.wm_class.eq_ignore_ascii_case(&self.class) {
            return Some(Rank::WmClass);
        }
        if self
            .url
            .as_ref()
            .is_some_and(|u| e.exec.contains(u.as_str()))
        {
            return Some(Rank::WebApp);
        }
        // Guarded: with no executable known, an entry with an empty `Exec=` would match.
        let first = e
            .exec
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches('"');
        if !self.exe.is_empty() && !first.is_empty() {
            if first == self.exe {
                return Some(Rank::ExePath);
            }
            if exe_name(first) == self.program && self.same_app_by_name(e) {
                return Some(Rank::ExeName);
            }
        }
        self.name
            .as_ref()
            .is_some_and(|n| *n == e.name)
            .then_some(Rank::Name)
    }
}

#[derive(Debug, Clone, Default)]
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
/// first file found is the one that shadows the rest, as in [`best_entries`]. One that
/// cannot be used — no `Name=` — sends the caller to the full scan, which treats it the
/// same way.
fn entry_named(dirs: &[PathBuf], app_id: &str) -> Option<DesktopEntry> {
    if app_id.is_empty() || app_id.contains('/') {
        return None;
    }
    let file = format!("{app_id}.desktop");
    let path = dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())?;
    parse_entry(app_id, &std::fs::read_to_string(path).ok()?)
}

/// The best-ranked application entry for each target, the first of equals in shadowing
/// order. One pass, reading one file at a time and keeping only the best so far for each
/// target, rather than every entry on the system; it stops early once every target has a
/// match nothing could beat. Hidden and `NoDisplay` entries still count: they are often
/// exactly the ones that name a helper window.
fn best_entries(dirs: &[PathBuf], targets: &[&Target]) -> Vec<Option<DesktopEntry>> {
    let mut best: Vec<Option<(Rank, DesktopEntry)>> = vec![None; targets.len()];
    let mut unsettled = targets.len();
    let mut seen = HashSet::new();
    'scan: for dir in dirs {
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
            for (slot, target) in best.iter_mut().zip(targets) {
                let Some(r) = target.rank(&entry) else {
                    continue;
                };
                if slot.as_ref().map_or(true, |(b, _)| r < *b) {
                    *slot = Some((r, entry.clone()));
                    if r == Rank::Id {
                        unsettled -= 1;
                    }
                }
            }
            if unsettled == 0 {
                break 'scan;
            }
        }
    }
    best.into_iter().map(|b| b.map(|(_, e)| e)).collect()
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

        // A window whose app id is `App` and whose executable is `/usr/bin/app`: one entry
        // matches each, and the app id is the surer.
        let mut window = Target::of_record("/usr/bin/app", None);
        window.class = "app".into();
        let found = best_entries(&dirs, &[&window]).pop().flatten().unwrap();
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
        let foot = Target::of_record("/usr/bin/foot", None);
        let scanned = best_entries(&dirs, &[&foot]).pop().flatten().unwrap();
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

    #[test]
    fn apps_recorded_without_an_icon_get_one_from_what_was_stored() {
        let whatsapp = entry(
            "WhatsApp",
            "omarchy-launch-webapp https://web.whatsapp.com/",
            "Icon=whatsapp",
        );
        let zen = entry(
            "Zen Browser",
            "/opt/zen-browser-bin/zen-bin %u",
            "StartupWMClass=zen\nIcon=zen-browser",
        );
        let t3 = entry("T3 Code", "t3code %U", "Icon=/icons/t3.png");
        let portal = entry("Portal", "/usr/libexec/some-other-path", "Icon=portal");
        // No `Exec=` at all: must not match an app known only by its id.
        let bare = "[Desktop Entry]\nName=Bare\nIcon=bare\n".to_string();
        let (_tmp, dirs) = dirs(
            &[("WhatsApp", &whatsapp), ("com.t3tools.T3Code", &t3)],
            &[("zen", &zen), ("portal", &portal), ("bare", &bare)],
        );
        let apps = [
            ("chrome-web.whatsapp.com__-Default", Some("WhatsApp")),
            ("/opt/zen-browser-bin/zen-bin", Some("Zen Browser")),
            ("com.t3tools.T3Code", Some("T3 Code")),
            ("/usr/lib/xdg-desktop-portal-gtk", Some("Portal")),
            ("/usr/bin/plymouthd", Some("plymouthd")),
            ("org.example.NoEntry", None),
        ]
        .map(|(k, n)| (k.to_string(), n.map(str::to_string)));

        let mut got = icons_in(&dirs, &apps);
        got.sort();
        let want = [
            ("/opt/zen-browser-bin/zen-bin", "zen-browser"),
            ("/usr/lib/xdg-desktop-portal-gtk", "portal"),
            ("chrome-web.whatsapp.com__-Default", "whatsapp"),
            ("com.t3tools.T3Code", "/icons/t3.png"),
        ]
        .map(|(k, i)| (k.to_string(), i.to_string()));
        assert_eq!(got, want);
    }

    #[test]
    fn a_second_install_of_the_same_program_does_not_take_the_others_icon() {
        let nightly = entry(
            "T3 Code (Nightly)",
            "\"/usr/lib/t3code-nightly/t3code\" %U",
            "Icon=nightly",
        );
        let server = entry("Foot Server", "foot --server", "Icon=foot-server");
        let foot = entry("Foot", "foot", "Icon=foot");
        let (_tmp, dirs) = dirs(
            &[("com.t3tools.T3Code", &nightly)],
            &[("foot-server", &server), ("foot", &foot)],
        );
        let apps = [
            ("/usr/lib/t3code/t3code", "T3 Code (Beta)"),
            ("/usr/lib/t3code-nightly/t3code", "T3 Code (Nightly)"),
            ("/usr/bin/foot", "Foot"),
        ]
        .map(|(k, n)| (k.to_string(), Some(n.to_string())));

        let mut got = icons_in(&dirs, &apps);
        got.sort();
        assert_eq!(
            got,
            [
                ("/usr/bin/foot".to_string(), "foot".to_string()),
                (
                    "/usr/lib/t3code-nightly/t3code".to_string(),
                    "nightly".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_detected_name_is_only_the_last_resort() {
        let by_name = entry("Foot", "something-else", "Icon=wrong");
        let by_exe = entry("Terminal", "/usr/bin/foot", "Icon=foot");
        let (_tmp, dirs) = dirs(&[("a", &by_name), ("b", &by_exe)], &[]);
        let foot = Target::of_record("/usr/bin/foot", Some("Foot"));
        let found = best_entries(&dirs, &[&foot]).pop().flatten().unwrap();
        assert_eq!(found.icon, "foot");
    }
}
