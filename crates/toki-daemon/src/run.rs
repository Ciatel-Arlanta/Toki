//! The collector loop.
//!
//! Three threads. The pump talks to the OS and sends facts. The resolver reads browser
//! history on its own schedule. This one is the only thing that decides anything, and the
//! only thing that writes sessions.
//!
//! It owns the `Tracker`, so all session assembly happens on one thread with no locking,
//! and the store connection lives here too — SQLite in WAL mode lets the resolver hold a
//! second connection for its updates.

use std::collections::HashMap;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, Months, Utc};
use tracing::{error, info, warn};

use toki_core::event::{AppKey, Millis};
use toki_core::resolve::Engine;
use toki_core::{Action, Config, Event, IdleHistogram, Tracker};
use toki_store::{OpenSession, Store};

use crate::platform::{audio, console, idle, ipc, pump, single, tray, window, Raw, Window};
use crate::resolver::{ResolverMsg, Schedule};
use crate::state::{PauseState, Shared};
use crate::{digest, logging, resolver};

/// How often presence is sampled. Fifteen seconds bounds how far past the idle threshold a
/// session can run before it is closed. The close is backdated to the last input, so the
/// sampling rate costs accuracy only in when the row appears, not in what it says.
const TICK: Duration = Duration::from_secs(15);
/// How often `last_seen` is written. Crash recovery closes a dangling session there, so
/// this is the worst-case over-count from a power cut.
const HEARTBEAT_MS: Millis = 60_000;
/// How long a failed weekly digest waits before the next try.
const DIGEST_RETRY_MS: Millis = 10 * 60_000;
/// How far wall time may move between one pass of the loop and the next before the loop is
/// taken not to have run across the gap. An arriving event only makes a pass happen sooner
/// than `TICK`, never later, so four missed ticks is well past anything scheduling jitter
/// on a loaded machine explains.
const MAX_LOOP_GAP_MS: Millis = 60_000;

pub fn run() -> Result<(), String> {
    // Before anything is written, because whether there is a console decides where the
    // first line goes.
    let console_attached = console::detach_if_ours();
    logging::init(console_attached);

    let _instance = single::Instance::acquire()
        .ok_or_else(|| "another toki-daemon is already running".to_string())?;
    toki_store::history::sweep_copies();

    let store =
        Store::open(&toki_store::db_path()).map_err(|e| format!("cannot open database: {e}"))?;
    info!(db = %toki_store::db_path().display(), "starting");

    // Written once rather than on the heartbeat: it cannot change within a run, and Settings
    // only shows it next to a liveness check that has already proved this run is the one
    // answering.
    if let Err(e) = store.set_daemon_pid(std::process::id()) {
        warn!(%e, "could not record the process id");
    }

    // Anything left open by a crash is closed at the last heartbeat before this run writes
    // a single row, so the two can never overlap.
    match store.recover() {
        Ok(Some(s)) => info!(
            app = s.app.as_str(),
            seconds = s.duration_ms() / 1_000,
            "recovered a session left open by the last run"
        ),
        Ok(None) => {}
        Err(e) => warn!(%e, "crash recovery failed"),
    }

    // Retention: 6 months of raw sessions, older data collapsed into daily_rollup — see
    // DESIGN.md §7. Run once per start rather than on its own timer; the daemon restarts
    // routinely (autostart on login, updates), which is a fine cadence for housekeeping
    // this coarse-grained.
    match store.rollup_before(retention_cutoff()) {
        Ok(0) => {}
        Ok(n) => info!(sessions = n, "rolled up sessions older than 6 months"),
        Err(e) => warn!(%e, "retention rollup failed"),
    }

    let resolver_tx = spawn_resolver();
    let shared = Arc::new(Shared::default());

    let (tx, rx) = channel::<Raw>();
    let ipc_thread = {
        let tx = tx.clone();
        let resolver_tx = resolver_tx.clone();
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("toki-ipc".into())
            .spawn(move || ipc::run(tx, resolver_tx, shared))
            .map_err(|e| format!("cannot start the IPC server: {e}"))?
    };
    let pump_thread = std::thread::Builder::new()
        .name("toki-pump".into())
        .spawn(move || pump::run(tx))
        .map_err(|e| format!("cannot start the event pump: {e}"))?;
    console::trap();

    // Held for the life of the loop so the audio probe is not entering and leaving a COM
    // apartment on every call.
    let _com = audio::Com::init();

    let mut collector = Collector::new(store, shared, resolver_tx.clone());
    collector.fill_missing_icons();
    loop {
        match rx.recv_timeout(TICK) {
            Ok(raw) => {
                // Before the event, not only on the tick below. Every `Raw` carries its own
                // wall-clock stamp, so one that arrived after the clock moved would close
                // the open session at the post-jump time and write the jump down as time
                // spent — the tick that would have caught it never gets to run first.
                collector.check_continuity(now());
                if collector.handle(raw) {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The pump has gone. Continuing would record time nobody is watching.
            Err(RecvTimeoutError::Disconnected) => break,
        }
        collector.tick(now());
    }

    collector.shutdown(now());
    let _ = resolver_tx.send(ResolverMsg::Stop);
    ipc::stop();
    pump::stop();
    let _ = pump_thread.join();
    let _ = ipc_thread.join();
    info!("stopped");
    Ok(())
}

fn now() -> Millis {
    Utc::now().timestamp_millis()
}

/// The window in front, unless it is a surface the pump never reports either: the shell,
/// the lock screen, Alt-Tab. Asked for whenever no event will say what is in front.
fn tracked_foreground() -> Option<Window> {
    pump::current_foreground().filter(|w| {
        !toki_core::ignore::is_default_ignored(&w.exe_path)
            && !toki_core::ignore::is_shell_chrome(&w.exe_path, &w.class)
    })
}

fn is_browser(w: &Window) -> bool {
    Engine::of_exe(&w.exe_path).is_some()
}

/// The offset in force right now, which is what gets stored on rows written now.
fn local_offset_min() -> i32 {
    Local::now().offset().local_minus_utc() / 60
}

/// Raw `sessions` rows older than this are eligible for `daily_rollup`. A calendar-month
/// subtraction, not a fixed day count, so "6 months" tracks what a person means by it
/// across months of different lengths.
fn retention_cutoff() -> Millis {
    Utc::now()
        .checked_sub_months(Months::new(6))
        .unwrap_or_else(|| Utc::now() - chrono::Duration::days(180))
        .timestamp_millis()
}

/// What the OS can tell us about an app beyond its path, read once.
struct AppInfo {
    engine: Option<Engine>,
}

struct Collector {
    store: Store,
    tracker: Tracker,
    apps: HashMap<String, AppInfo>,
    /// The window the open session belongs to, kept for the audio probe and for reopening
    /// after idle.
    current: Option<Window>,
    last_input: Millis,
    /// Wall clock as of the last pass of the run loop, so the next one can tell whether
    /// time moved further than a pass could account for — see
    /// [`Collector::check_continuity`].
    last_pass: Millis,
    next_heartbeat: Millis,
    /// When a timed pause ends. `None` while running, and also while paused indefinitely —
    /// the tracker is what knows which, so the two are never confused.
    ///
    /// Deliberately not persisted. A pause is a decision about the next hour, not about the
    /// installation, and one that survived a reboot would be a tracker that had silently
    /// stopped. Restarting the daemon resumes.
    paused_until: Option<Millis>,
    /// The open session as last written to `meta`, so it is only written again when it
    /// actually changes.
    mirrored: Option<OpenSession>,
    /// The idle histogram as last written, for the same reason.
    persisted_idle: IdleHistogram,
    /// Whether the last auxiliary `meta` write failed. Exists so a failing database is
    /// reported once rather than every minute — see [`Collector::aux_write`].
    aux_failing: bool,
    /// The IPC server's read side of the pause state. Updated alongside `paused_until`
    /// rather than derived from it on demand, since `get_state` runs on another thread.
    shared: Arc<Shared>,
    /// Told when a stretch of browsing ends, which is when it decides to read history.
    resolver: Sender<ResolverMsg>,
    /// No digest attempt before this. Set after one fails, so a balloon that cannot be
    /// shown is not rebuilt from the database on every focus change all Monday.
    digest_retry_at: Millis,
}

impl Collector {
    fn new(store: Store, shared: Arc<Shared>, resolver: Sender<ResolverMsg>) -> Self {
        let at = now();
        // Settings may have overridden the idle threshold on a previous run. Anything it
        // never touched keeps the default — this only ever narrows `Config::default()` by
        // one field, never invents the other two.
        let mut cfg = Config::default();
        if let Ok(Some(ms)) = store.idle_threshold_ms() {
            cfg.idle_threshold_ms = ms;
        }
        // Carry the absorbed-idle sample across restarts, so a week of it survives the
        // daemon stopping and starting rather than resetting to nothing each time. A
        // missing or unreadable value just starts the count over — this is measurement,
        // not data anyone would miss.
        let mut tracker = Tracker::new(cfg);
        if let Ok(Some(json)) = store.idle_histogram_json() {
            match serde_json::from_str(&json) {
                Ok(h) => tracker.set_idle_histogram(h),
                Err(e) => warn!(%e, "discarding unreadable idle histogram"),
            }
        }

        Self {
            store,
            persisted_idle: tracker.idle_histogram(),
            tracker,
            apps: HashMap::new(),
            current: None,
            last_input: idle::last_input_at(at),
            last_pass: at,
            aux_failing: false,
            next_heartbeat: at,
            paused_until: None,
            mirrored: None,
            shared,
            resolver,
            digest_retry_at: 0,
        }
    }

    /// Handle one fact from the pump. Returns true when the loop should end.
    fn handle(&mut self, raw: Raw) -> bool {
        match raw {
            Raw::Focus { at, window } => {
                self.focus(at, window);
                false
            }
            Raw::Suspend { at } | Raw::Lock { at } => {
                let actions = self.tracker.on_event(Event::Suspend { at });
                self.emit(actions);
                if self.current.as_ref().is_some_and(is_browser) {
                    self.browsing_ended();
                }
                self.current = None;
                self.mirror();
                false
            }
            // Windows does not reliably re-announce the foreground window after a wake or
            // an unlock, so it is asked directly rather than waited for.
            Raw::Resume { at } => {
                if let Some(w) = tracked_foreground() {
                    self.focus(at, w);
                }
                false
            }
            Raw::Pause { at, until } => {
                self.pause(at, until);
                false
            }
            Raw::Unpause { at } => {
                self.unpause(at);
                false
            }
            Raw::SetConfig { idle_threshold_ms } => {
                let mut cfg = *self.tracker.config();
                cfg.idle_threshold_ms = idle_threshold_ms;
                self.tracker.set_config(cfg);
                info!(
                    minutes = idle_threshold_ms / 60_000,
                    "idle threshold changed"
                );
                false
            }
            Raw::Stop { .. } => true,
        }
    }

    /// Stop recording. The open session is closed at `at`, so the paused span is absent from
    /// the day rather than credited to whatever was in front when it began.
    fn pause(&mut self, at: Millis, until: Option<Millis>) {
        let actions = self.tracker.on_event(Event::PauseStart { at });
        self.emit(actions);
        self.paused_until = until;
        self.mirror();
        tray::set_paused(true);
        self.shared.set(PauseState {
            paused: true,
            until,
        });
        match until {
            Some(u) => info!(minutes = (u - at) / 60_000, "paused"),
            None => info!("paused until resumed"),
        }
    }

    /// Start recording again, and pick up whatever is in front.
    ///
    /// Resuming changes no window and renames nothing, so — exactly as with a return from
    /// idle — no event arrives on its own and the foreground has to be asked for.
    fn unpause(&mut self, at: Millis) {
        if !self.tracker.is_paused() {
            return;
        }
        let actions = self.tracker.on_event(Event::PauseEnd { at });
        self.emit(actions);
        self.paused_until = None;
        tray::set_paused(false);
        self.shared.set(PauseState {
            paused: false,
            until: None,
        });
        info!("resumed");
        if let Some(w) = tracked_foreground() {
            self.focus(at, w);
        }
    }

    fn browsing_ended(&self) {
        let _ = self.resolver.send(ResolverMsg::BrowsingEnded);
    }

    /// Record a change of foreground window or caption.
    fn focus(&mut self, at: Millis, window: Window) {
        let left_browser = self.current.as_ref().is_some_and(is_browser) && !is_browser(&window);
        if left_browser {
            self.browsing_ended();
        }
        self.learn(&window);
        let actions = self.tracker.on_event(Event::Focus {
            at,
            app: AppKey::new(window.exe_path.clone()),
            title: window.title.clone(),
        });
        self.emit(actions);
        self.current = Some(window);
        self.mirror();
        self.maybe_show_weekly_digest(at);
    }

    /// Pop the weekly digest balloon on the first foreground activity of Monday — see
    /// DESIGN.md §10. `focus` fires on the very first foreground change of a session,
    /// including one right after boot or unlock, which is what "first foreground activity"
    /// means here; there is no separate timer to miss while asleep.
    fn maybe_show_weekly_digest(&mut self, at: Millis) {
        if at < self.digest_retry_at {
            return;
        }
        let offset = local_offset_min();
        let today = toki_core::time::local_date(at, offset);
        if today.weekday() != chrono::Weekday::Mon {
            return;
        }
        match self.store.last_digest_monday() {
            Ok(Some(last)) if last == today => return,
            Err(e) => {
                warn!(%e, "failed to read last digest date");
                return;
            }
            Ok(_) => {}
        }
        let digest = match digest::for_week_ending(&self.store, today) {
            Ok(d) => d,
            Err(e) => {
                warn!(%e, "weekly digest failed");
                self.digest_retry_at = at + DIGEST_RETRY_MS;
                return;
            }
        };

        // Not marked done when this fails: the tray icon didn't exist yet to show it on, so
        // a focus event after the back-off gets another try instead of the digest being
        // spent silently.
        let (title, body) = digest::format(&digest);
        if !tray::show_digest(&title, &body) {
            self.digest_retry_at = at + DIGEST_RETRY_MS;
            return;
        }
        if let Err(e) = self.store.set_last_digest_monday(today) {
            warn!(%e, "failed to record digest date");
        }
    }

    /// Close the open session when wall time has moved further than one pass of the loop
    /// could account for.
    ///
    /// Every timestamp in this daemon comes from the wall clock, `last_input_at` included —
    /// it is reported as `now - idle`. So a clock correction carries the input sample along
    /// with it and nothing downstream can tell the jump from ordinary activity: the open
    /// session simply absorbs the whole of it as time spent. This is not hypothetical. A
    /// boot with the RTC hours behind, corrected by NTP once the network came up, left a
    /// ten-hour Discord session in this machine's database on 2026-09-11.
    ///
    /// A machine that slept without telling us looks exactly the same from in here — modern
    /// standby gets no `PBT_APMSUSPEND` — and so does a process starved off the CPU long
    /// enough to miss its ticks. None of the three needs to be identified, because they
    /// want the same answer: the last pass the daemon actually made bounds what it can
    /// vouch for, so the session ends no later than that — earlier if input had already
    /// stopped before it, which [`Tracker::close_unaccounted`] works out. Whatever comes
    /// back afterwards reopens through the ordinary idle-return path.
    ///
    /// [`Tracker::on_input_return`] states the same rule where it belongs, in terms of the
    /// idle threshold rather than the loop. This is the half that can see a gap at all.
    fn check_continuity(&mut self, at: Millis) {
        let last = std::mem::replace(&mut self.last_pass, at);
        // Backwards as well as forwards: a clock moved back leaves `at - open.last_input`
        // shrinking instead of growing, so the session never reaches the threshold and runs
        // until the clock catches up with itself.
        if (0..=MAX_LOOP_GAP_MS).contains(&(at - last)) {
            return;
        }

        let actions = self.tracker.close_unaccounted(last);
        let closed = !actions.is_empty();
        self.emit(actions);
        // Whatever was in front before the gap may not be now, and no event will say so: a
        // machine waking from modern standby sends none. With nothing current, the return
        // of input reopens on the window actually in front (see `tick`).
        if closed {
            self.current = None;
        }
        self.mirror();
        // The mirror just changed and the next start recovers from it, so do not sit on
        // that for another half minute. Also unsticks a heartbeat scheduled past a clock
        // that has since moved back.
        self.next_heartbeat = at;
        warn!(
            seconds = (at - last) / 1_000,
            closed, "wall clock moved further than the loop could have"
        );
    }

    /// Presence sampling, the heartbeat, and the reopen that has to follow an idle close.
    fn tick(&mut self, at: Millis) {
        self.check_continuity(at);

        // A timed pause ends here rather than on a timer of its own. Fifteen seconds late
        // costs nothing when the shortest pause on offer is fifteen minutes.
        if self.paused_until.is_some_and(|until| at >= until) {
            self.unpause(at);
        }

        let seen = idle::last_input_at(at);

        let was_open = self.tracker.is_open();
        let actions = {
            let current = self.current.clone();
            self.tracker.on_tick(at, seen, || match &current {
                Some(w) => audio::is_rendering(w),
                None => false,
            })
        };
        self.emit(actions);
        if was_open && !self.tracker.is_open() && self.current.as_ref().is_some_and(is_browser) {
            self.browsing_ended();
        }

        // Coming back from idle changes no window and renames nothing, so no event fires.
        // Without this, the session that idle closed never reopens and the rest of the day
        // goes unrecorded. After a lock nothing is current: an unlock that found the lock
        // screen still in front had nothing to reopen, so the foreground is asked for here.
        if !self.tracker.is_open() && !self.tracker.is_paused() && seen > self.last_input {
            if let Some(w) = self.current.clone().or_else(tracked_foreground) {
                self.focus(seen, w);
            }
        }
        self.last_input = seen;
        self.mirror();

        if at >= self.next_heartbeat {
            self.heartbeat(at);
            self.next_heartbeat = at + HEARTBEAT_MS;
        }
    }

    /// Write `last_seen`, and the absorbed-idle counts if they moved. The counts ride the
    /// heartbeat rather than firing per tick: at heartbeat cadence a kill costs at most the
    /// last minute of counting.
    fn heartbeat(&mut self, at: Millis) {
        let idle = self.tracker.idle_histogram();
        let json = (idle != self.persisted_idle)
            .then(|| serde_json::to_string(&idle).ok())
            .flatten();
        let r = self.store.heartbeat(at, json.as_deref());
        if r.is_ok() && json.is_some() {
            self.persisted_idle = idle;
        }
        self.aux_write("heartbeat", r);
    }

    /// Close cleanly: at the moment we were told to stop, not at the last heartbeat, and
    /// with the mirror cleared so the next start finds nothing to recover.
    fn shutdown(&mut self, at: Millis) {
        let actions = self.tracker.on_event(Event::Shutdown { at });
        self.emit(actions);
        let r = self.store.set_open_session(None);
        self.aux_write("clear open session", r);
        // Not just on the timer: a clean stop should not throw away the counting done since
        // the last one.
        self.heartbeat(at);
    }

    /// Icons for apps recorded without one, once at start. An app learns its icon when it
    /// comes forward, so without this every app last used before icons were recorded, or
    /// not since its `.desktop` file arrived, would show its lettered tile until then.
    /// Apps with no entry at all are looked for again at each start, at the cost of one
    /// pass over the entries.
    fn fill_missing_icons(&mut self) {
        let apps = match self.store.apps_without_icon() {
            Ok(apps) if !apps.is_empty() => apps,
            Ok(_) => return,
            Err(e) => {
                warn!(%e, "cannot list apps without an icon");
                return;
            }
        };
        let found = window::recorded_icons(&apps);
        for (exe_path, icon) in &found {
            let r = self.store.set_detected_icon(exe_path, icon);
            self.aux_write("detected icon", r);
        }
        if !found.is_empty() {
            info!(
                found = found.len(),
                of = apps.len(),
                "icons found for recorded apps"
            );
        }
    }

    /// Read what the OS can tell us about an app, once, the first time it is seen.
    fn learn(&mut self, w: &Window) {
        if self.apps.contains_key(&w.exe_path) {
            return;
        }
        // Touches the disk, which is why it happens here rather than in the hook callback.
        let detected = window::detect(w);
        if let Some(name) = detected.name {
            let r = self.store.set_detected_name(&w.exe_path, &name);
            self.aux_write("detected name", r);
        }
        if let Some(icon) = detected.icon {
            let r = self.store.set_detected_icon(&w.exe_path, &icon);
            self.aux_write("detected icon", r);
        }
        self.apps.insert(
            w.exe_path.clone(),
            AppInfo {
                engine: Engine::of_exe(&w.exe_path),
            },
        );
    }

    /// Write what the tracker decided.
    ///
    /// The engine comes from the session's own app, not from whatever is in front now: a
    /// focus change hands back the action that closes the *previous* session.
    fn emit(&mut self, actions: Vec<Action>) {
        for action in &actions {
            let engine = match action {
                Action::Emit(s) => self.apps.get(s.app.as_str()).and_then(|a| a.engine),
                Action::ExtendLast { .. } => None,
            };
            if let Err(e) = self.store.apply(action, local_offset_min(), engine) {
                error!(%e, "write failed");
            }
        }
    }

    /// Report a failed auxiliary `meta` write — the heartbeat, the open-session mirror, and
    /// the two measurement rows.
    ///
    /// These deliberately don't abort anything the way a failed session write would: losing
    /// one is survivable. Losing them *silently* is not, which is what this fixes. The
    /// heartbeat and the open-session mirror together are the entire crash-recovery
    /// mechanism, so if they stop landing, the next start recovers a dangling session to a
    /// stale timestamp — or misses it altogether — and nothing anywhere says why.
    ///
    /// Logged on the transition rather than per call. All four writes go to one database
    /// and fail together, so a broken one would otherwise emit a line every minute,
    /// burying the first and most useful report under thousands of copies — and writing
    /// hard to a disk that may be full, which is a plausible reason it is failing at all.
    fn aux_write(&mut self, what: &'static str, r: toki_store::Result<()>) {
        match (r, self.aux_failing) {
            (Err(e), false) => {
                warn!(%e, what, "auxiliary write failed; crash recovery may be stale");
                self.aux_failing = true;
            }
            (Ok(()), true) => {
                info!(what, "auxiliary writes recovered");
                self.aux_failing = false;
            }
            _ => {}
        }
    }

    /// Keep `meta`'s copy of the in-progress session in step, writing only on a change.
    /// It is called on every tick and focus; most find the same session as last time, and
    /// each write is a commit.
    fn mirror(&mut self) {
        let open = match (self.tracker.is_open(), &self.current) {
            (true, Some(w)) => Some(OpenSession {
                start: self.tracker.open_start().unwrap_or_else(now),
                app: AppKey::new(w.exe_path.clone()),
                title: w.title.clone(),
                tz_offset_min: local_offset_min(),
                engine: self.apps.get(&w.exe_path).and_then(|a| a.engine),
            }),
            _ => None,
        };
        if open == self.mirrored {
            return;
        }
        let r = self.store.set_open_session(open.as_ref());
        // A failed write is tried again on the next call rather than taken as done.
        if r.is_ok() {
            self.mirrored = open;
        }
        self.aux_write("open session mirror", r);
    }
}

fn run_pass(store: &Store) {
    match resolver::pass(store) {
        Ok(s) if s.attempted > 0 => info!(
            resolved = s.resolved,
            missed = s.missed,
            not_a_page = s.not_a_page,
            attempted = s.attempted,
            hit = format!("{:.0}%", s.hit_rate() * 100.0),
            "resolver pass"
        ),
        Ok(_) => {}
        Err(e) => warn!(%e, "resolver"),
    }
}

/// The resolver runs on its own connection and its own clock.
///
/// Separate because a pass reads every browser's history, about 100 ms on this machine. On
/// the collector thread that would be foreground changes queued behind it every pass.
///
/// A pass runs at start, when the GUI asks (`RunNow`), [`resolver::SETTLE`] after a stretch
/// of browsing ends if the last pass is [`resolver::FLOOR`] old, and otherwise every
/// [`resolver::CEILING`]. Returns the sender side of its wake channel.
fn spawn_resolver() -> Sender<ResolverMsg> {
    let (tx, rx) = channel::<ResolverMsg>();
    let _ = std::thread::Builder::new()
        .name("toki-resolver".into())
        .spawn(move || {
            let Ok(store) = Store::open(&toki_store::db_path()) else {
                return;
            };
            // One at start, for whatever the last run left pending.
            run_pass(&store);
            let mut schedule = Schedule::after_pass(Instant::now());
            loop {
                match rx.recv_timeout(schedule.wait(Instant::now())) {
                    Ok(ResolverMsg::BrowsingEnded) => {
                        schedule.browsing_ended(Instant::now());
                        continue;
                    }
                    Ok(ResolverMsg::RunNow) | Err(RecvTimeoutError::Timeout) => {}
                    Ok(ResolverMsg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                }
                run_pass(&store);
                schedule = Schedule::after_pass(Instant::now());
            }
        });
    tx
}
