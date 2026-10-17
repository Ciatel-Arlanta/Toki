//! An app's icon on Linux: the image its `.desktop` entry names, found the way the desktop
//! finds it.
//!
//! The collector records each app's `Icon=` beside its name (`Store::detected_icon`), since
//! only it sees the window's app id. That value is an absolute path, or a name looked up per
//! the freedesktop Icon Theme Specification: in the user's theme, then each theme it
//! inherits from, then `hicolor`, then the loose files beside the themes and in `pixmaps`.
//!
//! What reaches the window is kept small. Icon sets ship PNGs up to 512 px and beyond, and
//! a webview holds every `<img>` decoded at full size, a megabyte apiece at 512 for a picture
//! drawn at 26. So a PNG wider than [`MAX_PX`] is scaled down here, once, and the large one
//! is dropped before the window sees it. SVG goes through as it is: the webview draws it at
//! the size it is shown.
//!
//! The lookup is all `stat`s against the theme's own directory list. No icon directory is
//! ever listed, so a theme of thousands of icons costs what its `index.theme` costs to hold.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::{as_uri, encode_png, sniff};

/// The widest image handed to the window. The largest icon drawn is 26 CSS px, which is 52
/// on a 2x display; 64 is the next size icon sets are drawn at.
const MAX_PX: u32 = 64;

/// What the theme is asked for: 32 px at scale 2, which is [`MAX_PX`] device pixels, and the
/// same 32 the Windows side reads out of an executable.
const SIZE: u32 = 32;
const SCALE: u32 = 2;

/// SVGs go to the webview whole, so one past this is skipped as a candidate. Real app icons
/// are a few KB to a few tens of KB; the outliers are illustrations, not icons.
const MAX_SVG_BYTES: u64 = 256 * 1024;

/// Larger than any PNG icon. Checked before reading, so a mistaken `Icon=` that names some
/// other large file costs nothing.
const MAX_PNG_BYTES: u64 = 4 * 1024 * 1024;

/// The most a PNG may make the decoder allocate: a 2048 px square, as 8-bit RGBA.
const MAX_DECODE_BYTES: usize = 2048 * 2048 * 4;

/// The icon the collector recorded for `exe_path`, as a `data:` URI, or `None` when there is
/// no record (the app was last seen before this existed, or has no `.desktop` file) or no
/// image to show for it.
pub fn data_uri(exe_path: &str) -> Option<String> {
    // Its own short-lived connection: asked once per app per window, and the cache in front
    // of this keeps the answer. An older collector's database has no `icon` column, which
    // errors here and reads as no icon.
    let icon = toki_store::Store::open_read_only(&toki_store::db_path())
        .ok()?
        .detected_icon(exe_path)
        .ok()??;
    let path = find(&icon, themes(), base_dirs())?;
    load(&path)
}

/// Where themes and loose icons live, in the order the specification gives: the legacy
/// `~/.icons`, the user's data directory, the system ones, then `pixmaps`.
fn base_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut dirs = Vec::new();
        dirs.extend(home.as_ref().map(|h| h.join(".icons")));
        match std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            Some(d) => dirs.push(d.join("icons")),
            None => dirs.extend(home.map(|h| h.join(".local/share/icons"))),
        }
        let system = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        dirs.extend(
            system
                .split(':')
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("icons")),
        );
        dirs.push(PathBuf::from("/usr/share/pixmaps"));
        dirs
    })
}

/// The user's theme and everything it falls back on, read once for the life of the window.
fn themes() -> &'static [Theme] {
    static THEMES: OnceLock<Vec<Theme>> = OnceLock::new();
    THEMES.get_or_init(|| chain(&theme_name(), base_dirs()))
}

/// The icon theme the desktop is using. GNOME's key first, which is the one Omarchy's theme
/// switcher and the GNOME and GTK settings tools write; then GTK 3's own settings file;
/// then the theme every system has.
fn theme_name() -> String {
    gsettings_icon_theme()
        .or_else(gtk_settings_icon_theme)
        .unwrap_or_else(|| "hicolor".into())
}

fn gsettings_icon_theme() -> Option<String> {
    use std::process::{Command, Stdio};
    let out = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    plain_theme_name(&String::from_utf8_lossy(&out.stdout))
}

fn gtk_settings_icon_theme() -> Option<String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let text = std::fs::read_to_string(config.join("gtk-3.0/settings.ini")).ok()?;
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "gtk-icon-theme-name")
            .then(|| plain_theme_name(value))
            .flatten()
    })
}

/// A theme name without the quotes `gsettings` and some `settings.ini` files put around it.
/// A theme name is a directory name, so one with a separator in it is refused.
fn plain_theme_name(raw: &str) -> Option<String> {
    let name = raw.trim().trim_matches(['\'', '"']).trim();
    (!name.is_empty() && !name.contains('/') && name != "." && name != "..")
        .then(|| name.to_string())
}

/// One icon theme, as its `index.theme` describes it.
#[derive(Debug)]
struct Theme {
    /// `<base>/<name>` for every base directory that has one. A theme may be spread across
    /// several, and the same subdirectory is looked for in each.
    roots: Vec<PathBuf>,
    dirs: Vec<Dir>,
    parents: Vec<String>,
}

/// One size directory of a theme, such as `48x48/apps` or `scalable/apps`.
#[derive(Debug, Clone, PartialEq)]
struct Dir {
    path: String,
    size: u32,
    scale: u32,
    min: u32,
    max: u32,
    threshold: u32,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fixed,
    Scalable,
    Threshold,
}

impl Dir {
    /// Whether an icon here is exactly what was asked for.
    fn matches(&self, size: u32, scale: u32) -> bool {
        if self.scale != scale {
            return false;
        }
        match self.kind {
            Kind::Fixed => self.size == size,
            Kind::Scalable => (self.min..=self.max).contains(&size),
            Kind::Threshold => (self.size.saturating_sub(self.threshold)
                ..=self.size + self.threshold)
                .contains(&size),
        }
    }

    /// How far an icon here is from what was asked for, in device pixels.
    fn distance(&self, size: u32, scale: u32) -> u32 {
        let want = size * scale;
        let (low, high) = match self.kind {
            Kind::Fixed => (self.size, self.size),
            Kind::Scalable => (self.min, self.max),
            Kind::Threshold => (
                self.size.saturating_sub(self.threshold),
                self.size + self.threshold,
            ),
        };
        let (low, high) = (low * self.scale, high * self.scale);
        if want < low {
            low - want
        } else {
            want.saturating_sub(high)
        }
    }
}

/// The theme called `name` and every theme it inherits from, depth first, each once, with
/// `hicolor` last if nothing else brought it in: the order the specification searches in.
fn chain(name: &str, bases: &[PathBuf]) -> Vec<Theme> {
    fn visit(name: &str, bases: &[PathBuf], seen: &mut HashSet<String>, out: &mut Vec<Theme>) {
        if !seen.insert(name.to_string()) {
            return;
        }
        let Some(theme) = load_theme(name, bases) else {
            return;
        };
        let parents = theme.parents.clone();
        out.push(theme);
        for parent in &parents {
            visit(parent, bases, seen, out);
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    visit(name, bases, &mut seen, &mut out);
    visit("hicolor", bases, &mut seen, &mut out);
    out
}

/// A theme is only a theme with an `index.theme`. The first one found describes it; the
/// others only add places to look.
fn load_theme(name: &str, bases: &[PathBuf]) -> Option<Theme> {
    plain_theme_name(name)?;
    let roots: Vec<PathBuf> = bases
        .iter()
        .map(|b| b.join(name))
        .filter(|r| r.is_dir())
        .collect();
    let text = roots
        .iter()
        .find_map(|r| std::fs::read_to_string(r.join("index.theme")).ok())?;
    let (dirs, parents) = parse_index(&text);
    Some(Theme {
        roots,
        dirs,
        parents,
    })
}

/// `Directories` (and KDE's `ScaledDirectories`) in their listed order, each with its own
/// group's keys, plus `Inherits`. A directory listed with no `Size` is skipped, as the
/// specification requires.
///
/// One pass, with each group's keys gathered as it is read: big themes list hundreds of
/// directories over thousands of lines, and matching each directory against every line
/// took longer than everything else the window does at start.
fn parse_index(text: &str) -> (Vec<Dir>, Vec<String>) {
    #[derive(Default)]
    struct Keys<'a> {
        size: Option<&'a str>,
        scale: Option<&'a str>,
        min: Option<&'a str>,
        max: Option<&'a str>,
        threshold: Option<&'a str>,
        kind: Option<&'a str>,
    }

    let mut order: Vec<&str> = Vec::new();
    let mut parents = Vec::new();
    let mut groups: HashMap<&str, Keys> = HashMap::new();
    let mut group = "";
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if group == "Icon Theme" {
            let list = || value.split(',').map(str::trim).filter(|s| !s.is_empty());
            match key {
                "Directories" | "ScaledDirectories" => order.extend(list()),
                "Inherits" => parents.extend(list().map(str::to_string)),
                _ => {}
            }
            continue;
        }
        if group.is_empty() {
            continue;
        }
        let keys = groups.entry(group).or_default();
        let slot = match key {
            "Size" => &mut keys.size,
            "Scale" => &mut keys.scale,
            "MinSize" => &mut keys.min,
            "MaxSize" => &mut keys.max,
            "Threshold" => &mut keys.threshold,
            "Type" => &mut keys.kind,
            _ => continue,
        };
        // The first of a repeated key holds, as in every other desktop-file reader.
        slot.get_or_insert(value);
    }

    let num = |v: Option<&str>| v.and_then(|v| v.parse::<u32>().ok());
    let mut seen = HashSet::new();
    let dirs = order
        .into_iter()
        .filter(|d| seen.insert(*d))
        .filter_map(|path| {
            let keys = groups.get(path)?;
            let size = num(keys.size)?;
            Some(Dir {
                path: path.to_string(),
                size,
                scale: num(keys.scale).unwrap_or(1).max(1),
                min: num(keys.min).unwrap_or(size),
                max: num(keys.max).unwrap_or(size),
                threshold: num(keys.threshold).unwrap_or(2),
                kind: match keys.kind {
                    Some("Fixed") => Kind::Fixed,
                    Some("Scalable") => Kind::Scalable,
                    _ => Kind::Threshold,
                },
            })
        })
        .collect();
    (dirs, parents)
}

/// The formats a webview can draw, in the order a theme's own are preferred. XPM, the
/// specification's third, is not one of them.
const EXTENSIONS: [&str; 2] = ["png", "svg"];

/// The file to draw for `icon`: an absolute path as it is, or a name looked up through
/// `themes`, then loose in each base directory.
fn find(icon: &str, themes: &[Theme], bases: &[PathBuf]) -> Option<PathBuf> {
    if icon.starts_with('/') {
        let path = PathBuf::from(icon);
        return usable(&path).then_some(path);
    }
    // Plenty of `.desktop` files name the file rather than the icon, and desktops accept it.
    let name = EXTENSIONS
        .iter()
        .chain(&["xpm"])
        .find_map(|ext| icon.strip_suffix(&format!(".{ext}")))
        .unwrap_or(icon);
    // Not a path, so it must not be able to step out of the icon directories.
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return None;
    }
    themes
        .iter()
        .find_map(|t| lookup(t, name))
        .or_else(|| loose(name, bases))
}

/// One theme's best file for `name`: the first that is exactly the size asked for, or
/// failing that the nearest. A single pass gives the specification's answer, since any
/// exact match beats every near one.
fn lookup(theme: &Theme, name: &str) -> Option<PathBuf> {
    let mut nearest: Option<(u32, PathBuf)> = None;
    for dir in &theme.dirs {
        for root in &theme.roots {
            for ext in EXTENSIONS {
                let path = root.join(&dir.path).join(format!("{name}.{ext}"));
                if !usable(&path) {
                    continue;
                }
                if dir.matches(SIZE, SCALE) {
                    return Some(path);
                }
                let d = dir.distance(SIZE, SCALE);
                if nearest.as_ref().map_or(true, |(best, _)| d < *best) {
                    nearest = Some((d, path));
                }
            }
        }
    }
    nearest.map(|(_, p)| p)
}

/// An icon outside any theme, straight in a base directory, as old apps install them into
/// `pixmaps`.
fn loose(name: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    bases.iter().find_map(|base| {
        EXTENSIONS
            .iter()
            .map(|ext| base.join(format!("{name}.{ext}")))
            .find(|p| usable(p))
    })
}

/// A file small enough to be an icon. An SVG past [`MAX_SVG_BYTES`] is passed over here, so
/// the search goes on to the theme's next best rather than ending with nothing.
fn usable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let cap = match path.extension().and_then(|e| e.to_str()) {
        Some("svg") => MAX_SVG_BYTES,
        _ => MAX_PNG_BYTES,
    };
    meta.is_file() && meta.len() <= cap
}

/// The file as a `data:` URI, if it is a PNG or SVG. Judged by its bytes, not its name,
/// since an absolute `Icon=` may name anything.
fn load(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    match sniff(&bytes)? {
        "image/png" => Some(as_uri("image/png", &fit_png(&bytes)?)),
        "image/svg+xml" if bytes.len() as u64 <= MAX_SVG_BYTES => {
            Some(as_uri("image/svg+xml", &bytes))
        }
        _ => None,
    }
}

/// The PNG itself when it is already small enough, untouched; otherwise a copy scaled to
/// fit [`MAX_PX`]. Only the header is read to decide.
fn fit_png(bytes: &[u8]) -> Option<Cow<'_, [u8]>> {
    let mut decoder = png::Decoder::new_with_limits(
        bytes,
        png::Limits {
            bytes: MAX_DECODE_BYTES,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 {
        return None;
    }
    if w <= MAX_PX && h <= MAX_PX {
        return Some(Cow::Borrowed(bytes));
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).ok()?;
    let rgba = to_rgba(&buf[..frame.buffer_size()], frame.color_type)?;
    drop(buf);
    let (tw, th) = fitted(w, h);
    let small = shrink(&rgba, w, h, tw, th);
    encode_png(tw, th, &small).map(Cow::Owned)
}

/// The size that fits `w` x `h` inside [`MAX_PX`] square, keeping its shape.
fn fitted(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h) as u64;
    let scale = |n: u32| ((n as u64 * MAX_PX as u64 + long / 2) / long).max(1) as u32;
    (scale(w), scale(h))
}

/// 8-bit pixels of any colour type `normalize_to_color8` leaves, as RGBA.
fn to_rgba(px: &[u8], color: png::ColorType) -> Option<Vec<u8>> {
    use png::ColorType::*;
    Some(match color {
        Rgba => px.to_vec(),
        Rgb => px
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        GrayscaleAlpha => px
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        // Expanded to RGB or RGBA by the decoder; never seen here.
        Indexed => return None,
    })
}

/// Area-average straight RGBA from `w` x `h` down to `tw` x `th`. Colour is averaged
/// weighted by alpha, so the transparent surround an icon sits in cannot darken its edges.
fn shrink(src: &[u8], w: u32, h: u32, tw: u32, th: u32) -> Vec<u8> {
    let (sx, sy) = (w as f32 / tw as f32, h as f32 / th as f32);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        let (top, bottom) = (y as f32 * sy, (y + 1) as f32 * sy);
        for x in 0..tw {
            let (left, right) = (x as f32 * sx, (x + 1) as f32 * sx);
            let mut acc = [0f32; 4];
            for py in top.floor() as u32..(bottom.ceil() as u32).min(h) {
                let wy = bottom.min(py as f32 + 1.0) - top.max(py as f32);
                for px in left.floor() as u32..(right.ceil() as u32).min(w) {
                    let wx = right.min(px as f32 + 1.0) - left.max(px as f32);
                    let p = &src[((py * w + px) * 4) as usize..][..4];
                    let a = p[3] as f32 * wx * wy;
                    for c in 0..3 {
                        acc[c] += p[c] as f32 * a;
                    }
                    acc[3] += a;
                }
            }
            if acc[3] > 0.0 {
                let i = ((y * tw + x) * 4) as usize;
                for c in 0..3 {
                    out[i + c] = (acc[c] / acc[3]).round() as u8;
                }
                out[i + 3] = (acc[3] / (sx * sy)).round().min(255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory removed on drop; the crate has no dev-dependency for one.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "toki-icon-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn png_of(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let px: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
        encode_png(w, h, &px).unwrap()
    }

    fn size_of_png(bytes: &[u8]) -> (u32, u32) {
        let reader = png::Decoder::new(bytes).read_info().unwrap();
        (reader.info().width, reader.info().height)
    }

    const HICOLOR: &str = "[Icon Theme]\nName=Hicolor\nDirectories=16x16/apps,48x48/apps,256x256/apps,scalable/apps\n\n[16x16/apps]\nSize=16\nType=Threshold\n\n[48x48/apps]\nSize=48\nType=Threshold\n\n[256x256/apps]\nSize=256\nType=Threshold\n\n[scalable/apps]\nSize=16\nMinSize=8\nMaxSize=512\nType=Scalable\n";

    #[test]
    fn an_index_lists_its_directories_in_order_with_their_defaults() {
        let text = "# comment\n[Icon Theme]\nName=Yaru-blue\nInherits=Yaru, Humanity,hicolor\nDirectories=16x16/apps,32x32@2x/apps,nosize/apps,scalable/apps\n\n[16x16/apps]\nSize=16\nType=Fixed\n\n[32x32@2x/apps]\nSize=32\nScale=2\nType=Fixed\n\n[nosize/apps]\nType=Fixed\n\n[scalable/apps]\nSize=16\nMinSize=8\nMaxSize=512\nType=Scalable\n";
        let (dirs, parents) = parse_index(text);
        assert_eq!(parents, ["Yaru", "Humanity", "hicolor"]);
        let paths: Vec<_> = dirs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["16x16/apps", "32x32@2x/apps", "scalable/apps"]);
        assert_eq!(dirs[1].scale, 2);
        assert_eq!(
            (dirs[2].min, dirs[2].max, dirs[2].kind),
            (8, 512, Kind::Scalable)
        );
        // Threshold, the specification's default type, with its default slack.
        let (dirs, _) = parse_index("[Icon Theme]\nDirectories=a\n[a]\nSize=24\n");
        assert_eq!((dirs[0].kind, dirs[0].threshold), (Kind::Threshold, 2));
    }

    #[test]
    fn sizes_are_matched_and_measured_in_device_pixels() {
        let fixed = |size, scale| Dir {
            path: String::new(),
            size,
            scale,
            min: size,
            max: size,
            threshold: 2,
            kind: Kind::Fixed,
        };
        assert!(fixed(32, 2).matches(32, 2));
        assert!(!fixed(64, 1).matches(32, 2));
        assert_eq!(fixed(64, 1).distance(32, 2), 0);
        assert_eq!(fixed(48, 1).distance(32, 2), 16);
        assert_eq!(fixed(256, 1).distance(32, 2), 192);
    }

    #[test]
    fn the_nearest_size_wins_when_none_is_exact() {
        let tmp = Scratch::new();
        tmp.write("icons/hicolor/index.theme", HICOLOR.as_bytes());
        tmp.write("icons/hicolor/16x16/apps/app.png", &png_of(16, 16, [1; 4]));
        let want = tmp.write("icons/hicolor/48x48/apps/app.png", &png_of(48, 48, [1; 4]));
        tmp.write("icons/hicolor/256x256/apps/app.png", &png_of(4, 4, [1; 4]));
        let bases = [tmp.0.join("icons")];

        assert_eq!(find("app", &chain("hicolor", &bases), &bases), Some(want));
    }

    #[test]
    fn a_scalable_icon_fits_any_size() {
        let tmp = Scratch::new();
        tmp.write("icons/hicolor/index.theme", HICOLOR.as_bytes());
        tmp.write("icons/hicolor/48x48/apps/app.png", &png_of(48, 48, [1; 4]));
        let want = tmp.write("icons/hicolor/scalable/apps/app.svg", b"<svg/>");
        let bases = [tmp.0.join("icons")];

        assert_eq!(find("app", &chain("hicolor", &bases), &bases), Some(want));
    }

    #[test]
    fn the_users_theme_comes_first_then_what_it_inherits_then_hicolor() {
        let tmp = Scratch::new();
        tmp.write(
            "user/Mine/index.theme",
            b"[Icon Theme]\nInherits=Base\nDirectories=apps\n[apps]\nSize=32\nScale=2\nType=Fixed\n",
        );
        tmp.write(
            "sys/Base/index.theme",
            b"[Icon Theme]\nDirectories=apps\n[apps]\nSize=64\nType=Fixed\n",
        );
        tmp.write("sys/hicolor/index.theme", HICOLOR.as_bytes());
        let mine = tmp.write("user/Mine/apps/shared.png", &png_of(2, 2, [1; 4]));
        tmp.write("sys/Base/apps/shared.png", &png_of(2, 2, [1; 4]));
        let base = tmp.write("sys/Base/apps/inherited.svg", b"<svg/>");
        let hicolor = tmp.write(
            "sys/hicolor/48x48/apps/only-hicolor.png",
            &png_of(2, 2, [1; 4]),
        );
        let loose = tmp.write("pixmaps/legacy.png", &png_of(2, 2, [1; 4]));
        let bases = [tmp.0.join("user"), tmp.0.join("sys"), tmp.0.join("pixmaps")];
        let themes = chain("Mine", &bases);
        let names: Vec<_> = themes.iter().map(|t| t.roots[0].clone()).collect();
        assert_eq!(names.len(), 3, "Mine, Base, hicolor: {names:?}");

        assert_eq!(find("shared", &themes, &bases), Some(mine));
        assert_eq!(find("inherited", &themes, &bases), Some(base));
        assert_eq!(find("only-hicolor", &themes, &bases), Some(hicolor));
        assert_eq!(find("legacy", &themes, &bases), Some(loose));
        assert_eq!(
            find("legacy.png", &themes, &bases),
            find("legacy", &themes, &bases)
        );
        assert_eq!(find("absent", &themes, &bases), None);
    }

    #[test]
    fn a_theme_that_inherits_itself_is_read_once() {
        let tmp = Scratch::new();
        tmp.write(
            "icons/Loop/index.theme",
            b"[Icon Theme]\nInherits=Loop,hicolor\nDirectories=\n",
        );
        tmp.write("icons/hicolor/index.theme", HICOLOR.as_bytes());
        assert_eq!(chain("Loop", &[tmp.0.join("icons")]).len(), 2);
    }

    #[test]
    fn a_name_cannot_step_out_of_the_icon_directories() {
        let tmp = Scratch::new();
        tmp.write("icons/hicolor/index.theme", HICOLOR.as_bytes());
        tmp.write("secret.png", &png_of(2, 2, [1; 4]));
        let bases = [tmp.0.join("icons")];
        let themes = chain("hicolor", &bases);
        for name in ["../secret", "../../secret", ".hidden", "", "a/b"] {
            assert_eq!(find(name, &themes, &bases), None, "{name}");
        }
        assert_eq!(plain_theme_name("'../etc'"), None);
        assert_eq!(
            plain_theme_name("'Yaru-blue'\n").as_deref(),
            Some("Yaru-blue")
        );
    }

    #[test]
    fn an_absolute_path_is_used_as_it_is() {
        let tmp = Scratch::new();
        let path = tmp.write("anywhere/app.desktop.png", &png_of(8, 8, [1; 4]));
        let found = find(path.to_str().unwrap(), &[], &[]).unwrap();
        assert_eq!(found, path);
        assert!(load(&found)
            .unwrap()
            .starts_with("data:image/png;base64,iVBOR"));
        assert_eq!(find("/no/such/icon.png", &[], &[]), None);
    }

    #[test]
    fn an_oversized_svg_is_passed_over_for_the_next_best() {
        let tmp = Scratch::new();
        tmp.write("icons/hicolor/index.theme", HICOLOR.as_bytes());
        let mut big = b"<svg>".to_vec();
        big.resize(MAX_SVG_BYTES as usize + 1, b' ');
        tmp.write("icons/hicolor/scalable/apps/app.svg", &big);
        let want = tmp.write("icons/hicolor/48x48/apps/app.png", &png_of(48, 48, [1; 4]));
        let bases = [tmp.0.join("icons")];
        assert_eq!(find("app", &chain("hicolor", &bases), &bases), Some(want));
    }

    #[test]
    fn a_small_png_is_passed_through_and_a_large_one_scaled_to_fit() {
        let small = png_of(48, 48, [10, 20, 30, 255]);
        assert!(matches!(fit_png(&small), Some(Cow::Borrowed(_))));

        let large = png_of(512, 256, [10, 20, 30, 255]);
        let fit = fit_png(&large).unwrap();
        assert_eq!(size_of_png(&fit), (64, 32));
        assert!(fit.len() < large.len());

        assert!(fit_png(b"\x89PNG\r\n\x1a\nnot really").is_none());
    }

    #[test]
    fn shrinking_keeps_colour_and_lets_transparency_thin_it_not_darken_it() {
        // One opaque red pixel among three clear black ones.
        let src = [255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(shrink(&src, 2, 2, 1, 1), [255, 0, 0, 64]);

        let solid: Vec<u8> = (0..9).flat_map(|_| [7, 8, 9, 255]).collect();
        assert_eq!(shrink(&solid, 3, 3, 2, 2), [7, 8, 9, 255].repeat(4));
    }

    #[test]
    fn every_colour_type_becomes_rgba() {
        use png::ColorType::*;
        assert_eq!(to_rgba(&[1, 2, 3], Rgb).unwrap(), [1, 2, 3, 255]);
        assert_eq!(to_rgba(&[9, 128], GrayscaleAlpha).unwrap(), [9, 9, 9, 128]);
        assert_eq!(to_rgba(&[9], Grayscale).unwrap(), [9, 9, 9, 255]);
        assert!(to_rgba(&[0], Indexed).is_none());
    }

    #[test]
    fn a_shape_keeps_its_proportions_when_fitted() {
        assert_eq!(fitted(512, 512), (64, 64));
        assert_eq!(fitted(512, 256), (64, 32));
        assert_eq!(fitted(4000, 1), (64, 1));
    }
}
