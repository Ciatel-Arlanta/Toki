//! Reading the *browsers'* SQLite, as opposed to Toki's own.
//!
//! Both engines keep every visit with its time, and the page's URL and title. This module
//! streams those visits into a [`VisitMatcher`]; deciding which visit names which session
//! is [`toki_core::resolve`]'s job.
//!
//! The live file is read in place whenever that is safe, so a pass normally writes nothing:
//! - Any history whose browser leaves it unlocked, and every one whose browser is closed,
//!   takes an ordinary read-only connection. SQLite's locking gives it a consistent snapshot.
//! - A running Chromium holds its history under an exclusive lock, and so does a running
//!   Gecko browser on Linux, so a locked open fails. Such a file is read without locks and
//!   validated like a seqlock: only if it and its WAL had been quiet for [`QUIET`] before the
//!   read, and only if their write times and sizes are unchanged after. Under exclusive
//!   locking SQLite does not bump the header's change counter on commit (measured), but
//!   every commit moves a file's write time, open handle or not.
//! - Anything else falls back to copying the file with its journal or WAL, as before.
//!
//! An unlocked read is undone if it fails validation: the matcher's results are snapshotted
//! before it and restored after, so a discarded read leaves nothing behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use rusqlite::config::DbConfig;
use rusqlite::{Connection, ErrorCode, OpenFlags, Result};
use toki_core::event::Millis;
use toki_core::resolve::{Engine, VisitMatcher};

/// Microseconds between 1601-01-01 and 1970-01-01. Chromium counts from the former.
const WEBKIT_EPOCH_OFFSET_US: i64 = 11_644_473_600_000_000;

/// How long a history must have gone unwritten before it is read without locks. A commit
/// writes its pages in a burst of milliseconds, so this is far longer than one takes.
pub(crate) const QUIET: Duration = Duration::from_secs(2);

/// How each source was read since the process started, for `toki-daemon bench`.
pub static READS_LOCKED: AtomicU64 = AtomicU64::new(0);
pub static READS_UNLOCKED: AtomicU64 = AtomicU64::new(0);
pub static READS_COPIED: AtomicU64 = AtomicU64::new(0);

/// Stream one history database's visits into `matcher`, returning how many were read.
///
/// Failure is not fatal to the caller: a browser mid-upgrade, a profile that has never been
/// opened, or a schema change all just mean this source contributes nothing this pass.
pub fn match_visits(path: &Path, engine: Engine, matcher: &mut VisitMatcher) -> Result<usize> {
    read_in_place(path, engine, matcher, QUIET).or_else(|_| read_copy(path, engine, matcher))
}

/// A locked read-only open first; on a lock held by the browser, a validated unlocked one.
fn read_in_place(
    path: &Path,
    engine: Engine,
    matcher: &mut VisitMatcher,
    quiet: Duration,
) -> Result<usize> {
    // A lock held by the browser fails the first step, before any row is offered.
    let locked = open_live(path, "mode=ro")?;
    match read_visits(&locked, engine, matcher) {
        Err(e) if is_locked(&e) => {}
        result => {
            READS_LOCKED.fetch_add(1, Ordering::Relaxed);
            return result;
        }
    }
    drop(locked);

    let kept = matcher.snapshot();
    let checked = read_unlocked(path, quiet, |conn| read_visits(conn, engine, matcher));
    if checked.is_err() {
        matcher.restore(kept);
    }
    READS_UNLOCKED.fetch_add(u64::from(checked.is_ok()), Ordering::Relaxed);
    checked
}

/// Run `read` against a browser's file without taking its locks, and keep the answer only if
/// neither the file nor its WAL had been written for `quiet` before, nor was written during.
/// A failed check is an error: the caller discards whatever `read` gathered and falls back
/// to a copy.
pub(crate) fn read_unlocked<T>(
    path: &Path,
    quiet: Duration,
    read: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let before = quiet_stamp(path, quiet)?;
    let conn = open_unlocked(path)?;
    let out = read(&conn)?;
    drop(conn);
    if stamp(path)? != before {
        return Err(io_error("written during the read"));
    }
    Ok(out)
}

/// The fallback: a private copy with its journal or WAL, opened writable so SQLite can roll
/// a hot journal back to the last committed state.
fn read_copy(path: &Path, engine: Engine, matcher: &mut VisitMatcher) -> Result<usize> {
    let copy = TempCopy::of(path)?;
    let conn = Connection::open_with_flags(&copy.path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let result = read_visits(&conn, engine, matcher)?;
    READS_COPIED.fetch_add(1, Ordering::Relaxed);
    Ok(result)
}

/// The browser's own file, read-only, through a URI so the options can be passed. No busy
/// wait: a browser's lock is held for as long as it runs, so waiting buys nothing.
pub(crate) fn open_live(path: &Path, options: &str) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        format!("{}?{options}", file_uri(path)),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::ZERO)?;
    Ok(conn)
}

/// The browser's own file, read-only and taking no locks, for a history its browser holds
/// exclusively.
///
/// On Windows that is `nolock=1`. On Linux it is the `unix-none` VFS instead, because a
/// Gecko history is in WAL mode and `nolock=1` makes SQLite refuse WAL outright
/// (`sqlite3PagerWalSupported`), which sent every pass to a full copy of the database and
/// its WAL while the browser ran: 7.5 MB for one Zen profile. `unix-none` takes no locks
/// either, but leaves WAL allowed, and exclusive locking mode, set before the first read,
/// keeps the WAL index in this connection's memory rather than in a `-shm` file the browser
/// never made. The WAL is read for what it is: its frames are checksummed, so recovery stops
/// at the last complete commit and a half-written one is never seen.
///
/// Nothing is written. The file is opened read-only at the OS level, a read-only WAL refuses
/// to checkpoint, and checkpoint-on-close is switched off besides, so closing cannot try to
/// fold the browser's WAL into its database or delete it.
fn open_unlocked(path: &Path) -> Result<Connection> {
    #[cfg(unix)]
    {
        let conn = open_live(path, "mode=ro&vfs=unix-none")?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        Ok(conn)
    }
    #[cfg(not(unix))]
    {
        open_live(path, "mode=ro&nolock=1")
    }
}

/// `file:` URI for a path. Windows separators become `/`, and the characters a URI gives
/// meaning to are escaped; profile folders contain spaces and parentheses.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for c in path.to_string_lossy().chars() {
        match c {
            '\\' => uri.push('/'),
            '%' => uri.push_str("%25"),
            ' ' => uri.push_str("%20"),
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            c => uri.push(c),
        }
    }
    uri
}

pub(crate) fn is_locked(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// What a write changes: the last-write time and size of the database, and of its WAL if
/// it has one. In WAL mode a commit only appends to the WAL; the database itself moves only
/// when a checkpoint folds the WAL back in.
type Stamp = [Option<(SystemTime, u64)>; 2];

fn stamp(path: &Path) -> Result<Stamp> {
    let of = |p: &Path| -> Result<Option<(SystemTime, u64)>> {
        match std::fs::metadata(p) {
            Ok(meta) => Ok(Some((meta.modified().map_err(to_sqlite_err)?, meta.len()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(to_sqlite_err(e)),
        }
    };
    let main = of(path)?.ok_or_else(|| io_error("history is missing"))?;
    Ok([Some(main), of(&append_ext(path, "-wal"))?])
}

/// The stamp, provided neither file has been written for `quiet`.
fn quiet_stamp(path: &Path, quiet: Duration) -> Result<Stamp> {
    let stamp = stamp(path)?;
    let now = SystemTime::now();
    let busy = stamp
        .iter()
        .flatten()
        .any(|(modified, _)| now.duration_since(*modified).unwrap_or_default() < quiet);
    if busy {
        return Err(io_error("history was written moments ago"));
    }
    Ok(stamp)
}

fn io_error(message: &str) -> rusqlite::Error {
    to_sqlite_err(std::io::Error::other(message))
}

/// The query half, separated so it can be tested against a synthetic database. Returns how
/// many visits were read.
///
/// Visits past the matcher's horizon stay in the database, since none can name a pending
/// session. The title is borrowed from SQLite's buffer and checked before the URL is parsed,
/// so the visits that are dropped, nearly all of them, cost no allocation.
pub fn read_visits(conn: &Connection, engine: Engine, matcher: &mut VisitMatcher) -> Result<usize> {
    let sql = match engine {
        Engine::Gecko => {
            "SELECT v.visit_date, p.url, p.title
             FROM moz_historyvisits v JOIN moz_places p ON p.id = v.place_id
             WHERE p.title <> '' AND v.visit_date <= ?1"
        }
        Engine::Chromium => {
            "SELECT v.visit_time, u.url, u.title
             FROM visits v JOIN urls u ON u.id = v.url
             WHERE u.title <> '' AND v.visit_time <= ?1"
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([from_millis(matcher.horizon(), engine)])?;
    let mut read = 0;
    while let Some(row) = rows.next()? {
        read += 1;
        let (Ok(title), Ok(url)) = (row.get_ref(2)?.as_str(), row.get_ref(1)?.as_str()) else {
            continue;
        };
        if !matcher.wants(title) {
            continue;
        }
        if let Some(host) = host_of(url) {
            matcher.offer(to_millis(row.get(0)?, engine), title, host);
        }
    }
    Ok(read)
}

/// Both engines store microseconds, but from different epochs.
fn to_millis(stamp: i64, engine: Engine) -> Millis {
    if stamp <= 0 {
        return 0;
    }
    match engine {
        Engine::Gecko => stamp / 1_000,
        Engine::Chromium => (stamp - WEBKIT_EPOCH_OFFSET_US) / 1_000,
    }
}

/// The inverse of [`to_millis`].
fn from_millis(ms: Millis, engine: Engine) -> i64 {
    match engine {
        Engine::Gecko => ms.saturating_mul(1_000),
        Engine::Chromium => ms
            .saturating_mul(1_000)
            .saturating_add(WEBKIT_EPOCH_OFFSET_US),
    }
}

/// Host of an `http`/`https` URL.
///
/// Returning `None` for every other scheme is the filter that keeps `chrome://`,
/// `about:`, `file://` and extension pages out of the index.
fn host_of(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // Anything before '@' is userinfo, not the host.
    let authority = authority.rsplit('@').next()?;

    let host = if let Some(after_bracket) = authority.strip_prefix('[') {
        // IPv6 literal: the colons inside are not a port separator.
        let end = after_bracket.find(']')?;
        &authority[..end + 2]
    } else {
        authority.split(':').next()?
    };

    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Files SQLite keeps beside a database: the WAL pair, or the rollback journal Chromium uses.
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

/// A copy of a database file, plus its sidecars, deleted on drop.
pub(crate) struct TempCopy {
    pub(crate) path: PathBuf,
}

/// Where fallback copies go: Toki's own folder rather than the shared temp directory, which
/// other programs and cleanup tools read, and which nothing of ours ever empties.
fn copies_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join("toki-test-copies");
    }
    crate::data_dir().join("history-copies")
}

/// Delete copies a killed process left behind; a clean one deletes its own on drop. The
/// daemon calls this once it holds the single-instance lock.
pub fn sweep_copies() {
    let Ok(entries) = std::fs::read_dir(copies_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

impl TempCopy {
    pub(crate) fn of(source: &Path) -> Result<Self> {
        let stem = source
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "history".to_string());
        let unique = format!(
            "toki-{}-{}-{stem}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let dir = copies_dir();
        std::fs::create_dir_all(&dir).map_err(to_sqlite_err)?;
        let path = dir.join(unique);

        let copied = std::fs::copy(source, &path).map_err(to_sqlite_err)?;
        BYTES_COPIED.fetch_add(copied, std::sync::atomic::Ordering::Relaxed);
        // A WAL holds everything written since the last checkpoint; a journal holds what an
        // unfinished transaction overwrote. Missing ones are normal: the browser may not be
        // running, and each engine uses only one kind.
        for ext in SIDECARS {
            let from = append_ext(source, ext);
            if from.is_file() {
                if let Ok(copied) = std::fs::copy(&from, append_ext(&path, ext)) {
                    BYTES_COPIED.fetch_add(copied, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        Ok(Self { path })
    }
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        for ext in SIDECARS {
            let _ = std::fs::remove_file(append_ext(&self.path, ext));
        }
    }
}

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bytes written by history copies since the process started. What `toki-daemon bench`
/// reports as the SSD cost of a pass.
pub static BYTES_COPIED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn append_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(ext);
    PathBuf::from(s)
}

fn to_sqlite_err(e: std::io::Error) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
        Some(e.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Millis = 60_000;
    /// 2026-08-27T00:00:00Z, so stamps look like the real thing.
    const T0: Millis = 1_787_788_800_000;

    const CHROMIUM_SCHEMA: &str = "
        CREATE TABLE urls (id INTEGER PRIMARY KEY, url TEXT, title TEXT);
        CREATE TABLE visits (id INTEGER PRIMARY KEY, url INTEGER, visit_time INTEGER);";

    /// One URL per row, each with one visit at the given time.
    fn chromium_db(conn: &Connection, rows: &[(&str, &str, Millis)]) {
        conn.execute_batch(CHROMIUM_SCHEMA).unwrap();
        for (url, title, at) in rows {
            conn.execute(
                "INSERT INTO urls (url, title) VALUES (?1, ?2)",
                rusqlite::params![url, title],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO visits (url, visit_time) VALUES (?1, ?2)",
                rusqlite::params![conn.last_insert_rowid(), from_millis(*at, Engine::Chromium)],
            )
            .unwrap();
        }
    }

    fn gecko_db(rows: &[(&str, &str, Millis)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT, title TEXT);
             CREATE TABLE moz_historyvisits (id INTEGER PRIMARY KEY, place_id INTEGER,
                                             visit_date INTEGER);",
        )
        .unwrap();
        for (url, title, at) in rows {
            conn.execute(
                "INSERT INTO moz_places (url, title) VALUES (?1, ?2)",
                rusqlite::params![url, title],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO moz_historyvisits (place_id, visit_date) VALUES (?1, ?2)",
                rusqlite::params![conn.last_insert_rowid(), from_millis(*at, Engine::Gecko)],
            )
            .unwrap();
        }
        conn
    }

    fn session(caption: &str, engine: Engine, end: Millis) -> VisitMatcher {
        VisitMatcher::new([(caption, engine, end)])
    }

    fn feed(conn: &Connection, engine: Engine, m: &mut VisitMatcher) -> usize {
        read_visits(conn, engine, m).unwrap()
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("toki-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Chromium's journal mode. A transaction that outgrows the page cache writes into the
    /// database file before it commits, leaving the old pages only in the journal.
    #[test]
    fn a_copy_taken_mid_transaction_reads_the_last_commit() {
        let dir = std::env::temp_dir().join(format!("toki-journal-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("History");
        let _ = std::fs::remove_file(&path);

        let browser = Connection::open(&path).unwrap();
        browser
            .execute_batch("PRAGMA journal_mode = TRUNCATE; PRAGMA cache_size = 1;")
            .unwrap();
        chromium_db(&browser, &[("https://claude.ai/", "Claude", T0)]);
        browser
            .execute_batch("BEGIN; UPDATE urls SET url = 'https://wrong.example/';")
            .unwrap();
        for i in 0..2_000 {
            browser
                .execute(
                    "INSERT INTO urls (url, title) VALUES ('https://filler.example/', ?1)",
                    [format!("filler {i} {}", "x".repeat(200))],
                )
                .unwrap();
        }
        assert!(append_ext(&path, "-journal").metadata().unwrap().len() > 0);

        let mut m = session("Claude - Google Chrome", Engine::Chromium, T0 + MIN);
        let read = read_copy(&path, Engine::Chromium, &mut m).unwrap();

        assert_eq!(read, 1);
        assert_eq!(m.host(0), Some("claude.ai"));
        drop(browser);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_extraction_covers_the_shapes_a_browser_actually_stores() {
        assert_eq!(
            host_of("https://www.youtube.com/watch?v=abc"),
            Some("www.youtube.com")
        );
        assert_eq!(host_of("http://example.com"), Some("example.com"));
        assert_eq!(
            host_of("https://example.com:8443/path"),
            Some("example.com")
        );
        assert_eq!(
            host_of("https://user:pw@example.com/x"),
            Some("example.com")
        );
        assert_eq!(host_of("https://example.com#frag"), Some("example.com"));
        assert_eq!(
            host_of("https://[2606:4700::1111]:443/x"),
            Some("[2606:4700::1111]")
        );
    }

    #[test]
    fn non_web_schemes_are_filtered_out_rather_than_stored() {
        assert_eq!(host_of("chrome://settings"), None);
        assert_eq!(host_of("about:config"), None);
        assert_eq!(host_of("file:///C:/notes.txt"), None);
        assert_eq!(host_of("moz-extension://abc/page.html"), None);
        assert_eq!(host_of("https://"), None);
    }

    #[test]
    fn gecko_microseconds_are_unix_based() {
        assert_eq!(to_millis(T0 * 1_000, Engine::Gecko), T0);
        assert_eq!(from_millis(T0, Engine::Gecko), T0 * 1_000);
    }

    #[test]
    fn chromium_microseconds_are_1601_based() {
        let webkit_us = T0 * 1_000 + WEBKIT_EPOCH_OFFSET_US;
        assert_eq!(to_millis(webkit_us, Engine::Chromium), T0);
        assert_eq!(from_millis(T0, Engine::Chromium), webkit_us);
    }

    #[test]
    fn visits_after_the_last_session_are_not_read() {
        let conn = Connection::open_in_memory().unwrap();
        chromium_db(
            &conn,
            &[
                ("https://checkout.stripe.com/pay", "X", T0 + 60 * MIN),
                ("https://x.com/home", "X", T0),
            ],
        );
        let mut m = session("X - Google Chrome", Engine::Chromium, T0 + MIN);
        assert_eq!(feed(&conn, Engine::Chromium, &mut m), 1);
        assert_eq!(m.host(0), Some("x.com"));
    }

    #[test]
    fn a_gecko_database_names_a_zen_window() {
        let conn = gecko_db(&[
            ("https://x.com/CodingNoobie", "Home / X", T0),
            ("chrome://browser/content/browser.xhtml", "Zen", T0),
        ]);
        let mut m = session("Home / X \u{2014} Zen Browser", Engine::Gecko, T0 + MIN);
        assert_eq!(feed(&conn, Engine::Gecko, &mut m), 2);
        assert_eq!(m.host(0), Some("x.com"));
    }

    #[test]
    fn several_profiles_feed_one_matcher() {
        let a = Connection::open_in_memory().unwrap();
        chromium_db(&a, &[("https://claude.ai/new", "New chat - Claude", T0)]);
        let b = Connection::open_in_memory().unwrap();
        chromium_db(&b, &[("https://github.com/x", "x - GitHub", T0)]);

        let mut m = VisitMatcher::new([
            (
                "New chat - Claude - Google Chrome",
                Engine::Chromium,
                T0 + MIN,
            ),
            ("x - GitHub - Google Chrome", Engine::Chromium, T0 + MIN),
        ]);
        feed(&a, Engine::Chromium, &mut m);
        feed(&b, Engine::Chromium, &mut m);

        assert_eq!(m.host(0), Some("claude.ai"));
        assert_eq!(m.host(1), Some("github.com"));
    }

    #[test]
    fn a_wal_history_in_use_is_read_in_place_at_its_last_commit() {
        let dir = scratch_dir("wal");
        let path = dir.join("places.sqlite");
        let browser = Connection::open(&path).unwrap();
        browser.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
        browser
            .execute_batch(
                "CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT, title TEXT);
                 CREATE TABLE moz_historyvisits (id INTEGER PRIMARY KEY, place_id INTEGER,
                                                 visit_date INTEGER);",
            )
            .unwrap();
        browser
            .execute(
                "INSERT INTO moz_places (url, title) VALUES ('https://x.com/', 'Home / X')",
                [],
            )
            .unwrap();
        browser
            .execute(
                "INSERT INTO moz_historyvisits (place_id, visit_date) VALUES (1, ?1)",
                [from_millis(T0, Engine::Gecko)],
            )
            .unwrap();
        // Uncommitted when the read happens: it must not be seen.
        browser
            .execute_batch("BEGIN; UPDATE moz_places SET url = 'https://wrong.example/';")
            .unwrap();

        let mut m = session("Home / X \u{2014} Zen Browser", Engine::Gecko, T0 + MIN);
        let before = READS_LOCKED.load(Ordering::Relaxed);
        match_visits(&path, Engine::Gecko, &mut m).unwrap();

        assert_eq!(m.host(0), Some("x.com"));
        assert!(READS_LOCKED.load(Ordering::Relaxed) > before);
        drop(browser);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Chromium's own setup: exclusive locking, held for as long as the browser runs.
    fn exclusively_held_history(dir: &Path) -> (PathBuf, Connection) {
        let path = dir.join("History");
        let browser = Connection::open(&path).unwrap();
        chromium_db(&browser, &[("https://claude.ai/", "Claude", T0)]);
        browser
            .execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")
            .unwrap();
        (path, browser)
    }

    #[test]
    fn an_exclusively_locked_history_is_read_without_locks_once_quiet() {
        let dir = scratch_dir("exclusive");
        let (path, browser) = exclusively_held_history(&dir);
        let mut m = session("Claude - Google Chrome", Engine::Chromium, T0 + MIN);

        let locked = open_live(&path, "mode=ro")
            .and_then(|c| read_visits(&c, Engine::Chromium, &mut m))
            .unwrap_err();
        assert!(is_locked(&locked));
        let read = read_in_place(&path, Engine::Chromium, &mut m, Duration::ZERO).unwrap();

        assert_eq!(read, 1);
        assert_eq!(m.host(0), Some("claude.ai"));
        drop(browser);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_history_written_moments_ago_is_not_read_without_locks() {
        let dir = scratch_dir("busy");
        let (path, browser) = exclusively_held_history(&dir);
        let mut m = session("Claude - Google Chrome", Engine::Chromium, T0 + MIN);

        assert!(read_in_place(&path, Engine::Chromium, &mut m, Duration::from_secs(3600)).is_err());
        assert_eq!(m.host(0), None);
        drop(browser);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Zen and Firefox's setup on Linux: exclusive locking entered before WAL, so the index
    /// lives in the browser's memory and there is no `-shm`, and nothing checkpointed yet, so
    /// the visit exists only in the WAL. A change is in flight, uncommitted.
    #[cfg(unix)]
    fn exclusively_held_wal_history(dir: &Path) -> (PathBuf, Connection) {
        let path = dir.join("places.sqlite");
        let browser = Connection::open(&path).unwrap();
        browser
            .execute_batch(
                "PRAGMA locking_mode = EXCLUSIVE;
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT, title TEXT);
                 CREATE TABLE moz_historyvisits (id INTEGER PRIMARY KEY, place_id INTEGER,
                                                 visit_date INTEGER);
                 INSERT INTO moz_places (url, title) VALUES ('https://x.com/', 'Home / X');",
            )
            .unwrap();
        browser
            .execute(
                "INSERT INTO moz_historyvisits (place_id, visit_date) VALUES (1, ?1)",
                [from_millis(T0, Engine::Gecko)],
            )
            .unwrap();
        browser
            .execute_batch("BEGIN; UPDATE moz_places SET url = 'https://wrong.example/';")
            .unwrap();
        (path, browser)
    }

    #[cfg(unix)]
    #[test]
    fn a_wal_history_held_exclusively_is_read_in_place_with_its_wal_untouched() {
        let dir = scratch_dir("gecko-exclusive");
        let (path, browser) = exclusively_held_wal_history(&dir);
        let wal = append_ext(&path, "-wal");
        let files = || (std::fs::read(&path).unwrap(), std::fs::read(&wal).unwrap());
        let before = files();
        assert!(!before.1.is_empty(), "the visit should be in the WAL only");
        let mut m = session("Home / X \u{2014} Zen Browser", Engine::Gecko, T0 + MIN);

        let locked = open_live(&path, "mode=ro")
            .and_then(|c| read_visits(&c, Engine::Gecko, &mut m))
            .unwrap_err();
        assert!(is_locked(&locked));
        let unlocked = READS_UNLOCKED.load(Ordering::Relaxed);
        let read = read_in_place(&path, Engine::Gecko, &mut m, Duration::ZERO).unwrap();

        assert_eq!(read, 1);
        assert_eq!(m.host(0), Some("x.com"));
        assert!(READS_UNLOCKED.load(Ordering::Relaxed) > unlocked);
        // Not a byte written, not a file made, and the browser carries on as it was.
        assert!(files() == before);
        assert!(!append_ext(&path, "-shm").exists());
        browser.execute_batch("COMMIT;").unwrap();
        drop(browser);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_wal_written_moments_ago_is_not_read_without_locks() {
        let dir = scratch_dir("gecko-busy");
        let (path, browser) = exclusively_held_wal_history(&dir);
        // The database itself long untouched: only its WAL is fresh.
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        let mut m = session("Home / X \u{2014} Zen Browser", Engine::Gecko, T0 + MIN);

        assert!(read_in_place(&path, Engine::Gecko, &mut m, Duration::from_secs(60)).is_err());
        assert_eq!(m.host(0), None);
        drop(browser);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_with_spaces_and_parentheses_opens_as_a_uri() {
        let dir = scratch_dir("uri test (release) 100%");
        let path = dir.join("History");
        chromium_db(
            &Connection::open(&path).unwrap(),
            &[("https://claude.ai/", "Claude", T0)],
        );
        let mut m = session("Claude - Google Chrome", Engine::Chromium, T0 + MIN);
        let conn = open_live(&path, "mode=ro").unwrap();
        assert_eq!(read_visits(&conn, Engine::Chromium, &mut m).unwrap(), 1);
        assert_eq!(m.host(0), Some("claude.ai"));
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let mut m = session("x - Google Chrome", Engine::Chromium, T0);
        let missing = std::env::temp_dir().join("toki-does-not-exist.sqlite");
        assert!(match_visits(&missing, Engine::Chromium, &mut m).is_err());
    }
}
