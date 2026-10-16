//! An app's own icon, for the rows that name it.
//!
//! Read out of the executable the shell would show in Explorer, drawn once per app and kept
//! for the life of the window. Nothing is written to disk: this crate owns no state
//! (lib.rs), and an icon costs a few milliseconds to read again.
//!
//! Only Windows has a source for these. Elsewhere `png` is always `None` and the UI keeps
//! its lettered tile, which is also what it shows for an app whose file has since gone.

#[cfg(windows)]
mod win;
#[cfg(windows)]
use win::rgba;

#[cfg(not(windows))]
fn rgba(_exe_path: &str) -> Option<(u32, u32, Vec<u8>)> {
    None
}

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// `exe_path -> data URI`, with `None` remembered too: an app with no readable icon would
/// otherwise be re-read on every page load.
static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

/// A `data:image/png;base64,…` URI, ready for an `<img src>`.
///
/// The lock is held across the read, so reads run one at a time. Every row asks at once on
/// first paint, and the shell's first lookups in a process fail when they race: all but one
/// come back empty, and the miss would then be remembered until the app restarts.
pub fn data_uri(exe_path: &str) -> Option<String> {
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    if let Some(hit) = cache.get(exe_path) {
        return hit.clone();
    }
    let uri = rgba(exe_path).and_then(|(w, h, px)| encode(w, h, &px));
    cache.insert(exe_path.to_owned(), uri.clone());
    uri
}

fn encode(width: u32, height: u32, rgba: &[u8]) -> Option<String> {
    let mut png = Vec::new();
    let mut enc = png::Encoder::new(&mut png, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().ok()?.write_image_data(rgba).ok()?;
    Some(format!("data:image/png;base64,{}", base64(&png)))
}

/// A site's icon for each of `domains` that a browser has one for, as `data:` URIs.
///
/// Read from the browsers' own favicon files, never fetched: see `toki_store::favicons` for
/// what that does and does not touch. Kept in memory for the life of the window, misses
/// included, so a site with no icon is not looked for again on every page load.
pub fn domain_data_uris(domains: Vec<String>) -> HashMap<String, Option<String>> {
    let cache = DOMAIN_CACHE.get_or_init(Default::default);
    let mut out = HashMap::new();
    let mut ask = Vec::new();
    if let Ok(known) = cache.lock() {
        for d in domains {
            match known.get(&d) {
                Some(hit) => {
                    out.insert(d, hit.clone());
                }
                None => ask.push(d),
            }
        }
    }
    if ask.is_empty() {
        return out;
    }
    let mut found = toki_store::favicons::read(&ask);
    for d in ask {
        let uri = found
            .remove(&d)
            .and_then(|b| sniff(&b).map(|m| as_uri(m, &b)));
        if let Ok(mut known) = cache.lock() {
            known.insert(d.clone(), uri.clone());
        }
        out.insert(d, uri);
    }
    out
}

static DOMAIN_CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

fn as_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64(bytes))
}

/// The image type, from the bytes: a browser stores whatever the site served, with no type
/// beside it. Anything unrecognised is left out rather than guessed at.
fn sniff(b: &[u8]) -> Option<&'static str> {
    Some(match b {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [0, 0, 1, 0, ..] => "image/x-icon",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        _ if looks_like_svg(b) => "image/svg+xml",
        _ => return None,
    })
}

fn looks_like_svg(b: &[u8]) -> bool {
    let head = &b[..b.len().min(512)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg"))
}

/// Standard base64. Not worth a dependency for the one place the crate needs it.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::data_uri;
    use super::{base64, sniff};

    #[test]
    fn it_names_an_image_by_its_bytes_and_refuses_anything_else() {
        assert_eq!(sniff(&[0x89, b'P', b'N', b'G', 13, 10]), Some("image/png"));
        assert_eq!(sniff(&[0, 0, 1, 0, 1, 0]), Some("image/x-icon"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"  <svg xmlns='x'/>"), Some("image/svg+xml"));
        assert_eq!(sniff(b"<?xml version='1.0'?><svg/>"), Some("image/svg+xml"));
        assert_eq!(sniff(b"<html><body>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[cfg(windows)]
    #[test]
    fn reads_a_real_icon_and_remembers_a_missing_one() {
        let uri = data_uri(r"C:\Windows\System32\notepad.exe").expect("notepad has an icon");
        assert!(uri.starts_with("data:image/png;base64,iVBOR"));
        assert!(uri.len() > 200);
        assert_eq!(data_uri(r"C:\no\such\app.exe"), None);
    }

    /// The first page asks for every row's icon at once; a cold shell fails all but one of a
    /// racing burst.
    #[cfg(windows)]
    #[test]
    fn reads_a_burst_of_icons_asked_for_at_once() {
        let exes: [&'static str; 8] = [
            r"C:\Windows\explorer.exe",
            r"C:\Windows\regedit.exe",
            r"C:\Windows\System32\cmd.exe",
            r"C:\Windows\System32\taskmgr.exe",
            r"C:\Windows\System32\mmc.exe",
            r"C:\Windows\System32\msinfo32.exe",
            r"C:\Windows\System32\charmap.exe",
            r"C:\Windows\System32\control.exe",
        ];
        let reads: Vec<_> = exes
            .iter()
            .map(|&exe| std::thread::spawn(move || (exe, data_uri(exe).is_some())))
            .collect();
        for read in reads {
            let (exe, found) = read.join().unwrap();
            assert!(found, "{exe} has an icon");
        }
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
