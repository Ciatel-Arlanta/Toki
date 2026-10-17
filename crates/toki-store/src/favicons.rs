//! A site's icon, read from the browser's own favicon database.
//!
//! Nothing is fetched: browsers already keep an icon for every page they have shown, in
//! `favicons.sqlite` (Gecko) or `Favicons` (Chromium) beside the history file, and this reads
//! those. It is the GUI's only use of them, and it is deliberately narrow:
//!
//! - It looks up the exact domains the caller names. The files are never listed or scanned
//!   for anything else, though they also hold icons for bookmarked pages.
//! - Page URLs stay inside the query. What comes back is icon bytes and nothing that says
//!   which page they came from.
//! - Nothing is kept. The caller holds the bytes in memory. A file the running browser holds
//!   locked is read in place without its locks when it has been quiet, exactly as history is
//!   (see `history`), and only otherwise copied, the copy deleted before this returns.
//!
//! Private windows never write favicons, so they are excluded the same way they are from
//! history.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params_from_iter, Connection, OpenFlags, OptionalExtension, Result};
use toki_core::resolve::Engine;

use crate::browsers::favicon_sources;
use crate::history::{is_locked, open_live, read_unlocked, TempCopy, QUIET};

/// Larger than any real favicon, which is a few KB. Anything past this is not one.
const MAX_BYTES: i64 = 256 * 1024;

/// Wide enough to stay sharp at 16px on a 2x display, and no wider than it needs to be.
const WANT_PX: i64 = 32;

/// The icon bytes for each of `domains` that any installed browser has one for. A domain
/// with no icon is simply absent, and so is every domain when no browser is installed.
pub fn read(domains: &[String]) -> HashMap<String, Vec<u8>> {
    let mut found = HashMap::new();
    for source in favicon_sources() {
        let missing: Vec<&String> = domains.iter().filter(|d| !found.contains_key(*d)).collect();
        if missing.is_empty() {
            break;
        }
        // A browser mid-upgrade or a profile that never loaded a page just contributes
        // nothing; the tile is the fallback.
        let _ = read_file(&source.path, source.engine, &missing, &mut found);
    }
    found
}

/// One favicon database: in place and read-only first; on the browser's lock, in place
/// without locks if the file is quiet; and failing that, a copy.
///
/// A running Zen holds a 5 MB database and a WAL of a couple more under its lock, and the
/// window asks once per batch of rows it draws, so the copy is the case worth avoiding.
fn read_file(
    path: &Path,
    engine: Engine,
    domains: &[&String],
    found: &mut HashMap<String, Vec<u8>>,
) -> Result<()> {
    let live = open_live(path, "mode=ro").and_then(|conn| lookup(&conn, engine, domains, found));
    match live {
        Err(e) if is_locked(&e) => {}
        done => return done,
    }
    // Into a map of its own, so a read that fails its check leaves nothing behind.
    let mut fresh = HashMap::new();
    if read_unlocked(path, QUIET, |conn| {
        lookup(conn, engine, domains, &mut fresh)
    })
    .is_ok()
    {
        found.extend(fresh);
        return Ok(());
    }
    let copy = TempCopy::of(path)?;
    // Writable so SQLite can roll back a hot journal, the same as the history reader.
    let conn = Connection::open_with_flags(&copy.path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    lookup(&conn, engine, domains, found)
}

fn lookup(
    conn: &Connection,
    engine: Engine,
    domains: &[&String],
    found: &mut HashMap<String, Vec<u8>>,
) -> Result<()> {
    let sql = match engine {
        Engine::Gecko => query(
            "moz_pages_w_icons p
             JOIN moz_icons_to_pages ip ON ip.page_id = p.id
             JOIN moz_icons i ON i.id = ip.icon_id",
            "p.page_url",
            "i.data",
            "i.width",
        ),
        Engine::Chromium => query(
            "icon_mapping m JOIN favicon_bitmaps b ON b.icon_id = m.icon_id",
            "m.page_url",
            "b.image_data",
            "b.width",
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    for domain in domains {
        let Some(patterns) = page_patterns(domain) else {
            continue;
        };
        let bytes: Option<Vec<u8>> = stmt
            .query_row(params_from_iter(patterns), |row| row.get(0))
            .optional()?;
        if let Some(bytes) = bytes {
            found.insert((*domain).clone(), bytes);
        }
    }
    Ok(())
}

/// The lookup shared by both schemas, which differ only in names. The pages are matched by
/// the start of their URL, with and without `www.`, over either scheme; the icon wanted is
/// the smallest that is at least `WANT_PX` wide, or failing that the largest there is.
fn query(from: &str, page: &str, data: &str, width: &str) -> String {
    format!(
        "SELECT {data} FROM {from}
         WHERE ({page} LIKE ?1 ESCAPE '\\' OR {page} LIKE ?2 ESCAPE '\\'
             OR {page} LIKE ?3 ESCAPE '\\' OR {page} LIKE ?4 ESCAPE '\\')
           AND {data} IS NOT NULL AND length({data}) <= {MAX_BYTES}
         ORDER BY ({width} >= {WANT_PX}) DESC,
                  CASE WHEN {width} >= {WANT_PX} THEN {width} END ASC,
                  {width} DESC
         LIMIT 1"
    )
}

/// `LIKE` patterns for pages on `domain`, or `None` if it is not a plausible host. Anchored
/// on the scheme and the slash after the host, so `github.com` cannot match `notgithub.com`
/// or `github.com.evil.test`.
fn page_patterns(domain: &str) -> Option<[String; 4]> {
    let ok = !domain.is_empty()
        && domain.len() <= 253
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    if !ok {
        return None;
    }
    let host = domain.to_ascii_lowercase();
    Some([
        format!("https://{host}/%"),
        format!("https://www.{host}/%"),
        format!("http://{host}/%"),
        format!("http://www.{host}/%"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gecko(rows: &[(&str, i64, Option<&[u8]>)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_pages_w_icons (id INTEGER PRIMARY KEY, page_url TEXT);
             CREATE TABLE moz_icons (id INTEGER PRIMARY KEY, width INTEGER, data BLOB);
             CREATE TABLE moz_icons_to_pages (page_id INTEGER, icon_id INTEGER);",
        )
        .unwrap();
        for (i, (url, width, data)) in rows.iter().enumerate() {
            let id = i as i64 + 1;
            conn.execute("INSERT INTO moz_pages_w_icons VALUES (?1, ?2)", (id, url))
                .unwrap();
            conn.execute(
                "INSERT INTO moz_icons VALUES (?1, ?2, ?3)",
                (id, width, data),
            )
            .unwrap();
            conn.execute("INSERT INTO moz_icons_to_pages VALUES (?1, ?1)", [id])
                .unwrap();
        }
        conn
    }

    fn chromium(rows: &[(&str, i64, Option<&[u8]>)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE icon_mapping (id INTEGER PRIMARY KEY, page_url TEXT, icon_id INTEGER);
             CREATE TABLE favicon_bitmaps (id INTEGER PRIMARY KEY, icon_id INTEGER,
                                           image_data BLOB, width INTEGER);",
        )
        .unwrap();
        for (i, (url, width, data)) in rows.iter().enumerate() {
            let id = i as i64 + 1;
            conn.execute("INSERT INTO icon_mapping VALUES (?1, ?2, ?1)", (id, url))
                .unwrap();
            conn.execute(
                "INSERT INTO favicon_bitmaps VALUES (?1, ?1, ?2, ?3)",
                (id, data, width),
            )
            .unwrap();
        }
        conn
    }

    fn ask(conn: &Connection, engine: Engine, domain: &str) -> Option<Vec<u8>> {
        let d = domain.to_string();
        let mut found = HashMap::new();
        lookup(conn, engine, &[&d], &mut found).unwrap();
        found.remove(domain)
    }

    const A: &[u8] = b"a";
    const B: &[u8] = b"b";

    #[test]
    fn both_schemas_find_a_domain_with_or_without_www_over_either_scheme() {
        for engine in [Engine::Gecko, Engine::Chromium] {
            let make = |rows: &[(&str, i64, Option<&[u8]>)]| match engine {
                Engine::Gecko => gecko(rows),
                Engine::Chromium => chromium(rows),
            };
            for url in [
                "https://github.com/rust-lang",
                "https://www.github.com/",
                "http://github.com/x?y=1",
            ] {
                let conn = make(&[(url, 32, Some(A))]);
                assert_eq!(ask(&conn, engine, "github.com"), Some(A.to_vec()), "{url}");
            }
        }
    }

    #[test]
    fn a_different_domain_never_matches_by_suffix_or_prefix() {
        for engine in [Engine::Gecko, Engine::Chromium] {
            let rows = [
                ("https://notgithub.com/", 32, Some(A)),
                ("https://github.com.evil.test/", 32, Some(A)),
                ("https://example.org/github.com/", 32, Some(A)),
            ];
            let conn = match engine {
                Engine::Gecko => gecko(&rows),
                Engine::Chromium => chromium(&rows),
            };
            assert_eq!(ask(&conn, engine, "github.com"), None);
        }
    }

    #[test]
    fn it_takes_the_smallest_icon_that_is_sharp_enough_else_the_biggest() {
        let conn = gecko(&[
            ("https://a.test/1", 16, Some(b"16")),
            ("https://a.test/2", 64, Some(b"64")),
            ("https://a.test/3", 32, Some(b"32")),
            ("https://b.test/1", 16, Some(b"16")),
            ("https://b.test/2", 24, Some(b"24")),
        ]);
        assert_eq!(ask(&conn, Engine::Gecko, "a.test"), Some(b"32".to_vec()));
        assert_eq!(ask(&conn, Engine::Gecko, "b.test"), Some(b"24".to_vec()));
    }

    #[test]
    fn an_icon_row_without_data_or_past_the_size_cap_is_not_an_icon() {
        let huge = vec![0u8; MAX_BYTES as usize + 1];
        let conn = chromium(&[
            ("https://a.test/", 32, None),
            ("https://b.test/", 32, Some(&huge)),
            ("https://c.test/", 32, Some(B)),
        ]);
        assert_eq!(ask(&conn, Engine::Chromium, "a.test"), None);
        assert_eq!(ask(&conn, Engine::Chromium, "b.test"), None);
        assert_eq!(ask(&conn, Engine::Chromium, "c.test"), Some(B.to_vec()));
    }

    #[test]
    fn a_name_that_is_not_a_host_is_never_put_in_a_query() {
        assert!(page_patterns("").is_none());
        assert!(page_patterns("a.test/path").is_none());
        assert!(page_patterns("a%.test").is_none());
        assert!(page_patterns("a_b.test").is_none());
        assert!(page_patterns("user@a.test").is_none());
        assert_eq!(
            page_patterns("GitHub.com").unwrap()[0],
            "https://github.com/%"
        );
    }

    #[test]
    fn reading_a_file_that_is_not_there_is_an_error_not_a_panic() {
        let missing = std::env::temp_dir().join("toki-no-such-favicons.sqlite");
        let d = "a.test".to_string();
        let mut found = HashMap::new();
        assert!(read_file(&missing, Engine::Gecko, &[&d], &mut found).is_err());
        assert!(found.is_empty());
    }

    /// Zen's favicons on Linux, as its history: exclusive locking entered before WAL, the
    /// icon only in the WAL, and the lock held for as long as the browser runs.
    #[cfg(unix)]
    #[test]
    fn a_running_browsers_locked_favicons_are_read_in_place() {
        let dir =
            std::env::temp_dir().join(format!("toki-favicons-locked-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("favicons.sqlite");
        let browser = Connection::open(&path).unwrap();
        browser
            .execute_batch(
                "PRAGMA locking_mode = EXCLUSIVE;
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE moz_pages_w_icons (id INTEGER PRIMARY KEY, page_url TEXT);
                 CREATE TABLE moz_icons (id INTEGER PRIMARY KEY, width INTEGER, data BLOB);
                 CREATE TABLE moz_icons_to_pages (page_id INTEGER, icon_id INTEGER);
                 INSERT INTO moz_pages_w_icons VALUES (1, 'https://a.test/');
                 INSERT INTO moz_icons VALUES (1, 32, x'0102');
                 INSERT INTO moz_icons_to_pages VALUES (1, 1);",
            )
            .unwrap();
        let d = "a.test".to_string();
        assert!(open_live(&path, "mode=ro")
            .and_then(|c| lookup(&c, Engine::Gecko, &[&d], &mut HashMap::new()))
            .is_err_and(|e| is_locked(&e)));

        let mut found = HashMap::new();
        read_unlocked(&path, std::time::Duration::ZERO, |c| {
            lookup(c, Engine::Gecko, &[&d], &mut found)
        })
        .unwrap();
        assert_eq!(found.get("a.test"), Some(&vec![1u8, 2]));
        assert!(!dir.join("favicons.sqlite-shm").exists());
        drop(browser);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_on_disk_is_read_through_the_same_path() {
        let dir = std::env::temp_dir().join(format!("toki-favicons-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("favicons.sqlite");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE moz_pages_w_icons (id INTEGER PRIMARY KEY, page_url TEXT);
                 CREATE TABLE moz_icons (id INTEGER PRIMARY KEY, width INTEGER, data BLOB);
                 CREATE TABLE moz_icons_to_pages (page_id INTEGER, icon_id INTEGER);
                 INSERT INTO moz_pages_w_icons VALUES (1, 'https://a.test/');
                 INSERT INTO moz_icons VALUES (1, 32, x'0102');
                 INSERT INTO moz_icons_to_pages VALUES (1, 1);",
            )
            .unwrap();
        }
        let d = "a.test".to_string();
        let mut found = HashMap::new();
        read_file(&path, Engine::Gecko, &[&d], &mut found).unwrap();
        assert_eq!(found.get("a.test"), Some(&vec![1u8, 2]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
