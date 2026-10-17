# Toki — Design

A local-first Windows screen-time tracker. Records which application holds the foreground
and, for browsers, which domain, into a private SQLite database. No network code in the daemon.

Target: the always-running component stays under 5 MB RSS and does no work when nothing
changes on screen.

---

## 1. Non-goals

Stated up front because they shape everything below.

- **No cloud, no telemetry, no accounts.** Neither binary links an HTTP client. The one
  request Toki makes is the window's daily update check (§12).
- **No exact-URL tracking.** Domain granularity only. "3h on youtube.com", never which video.
- **No limits, blocking, or nudges.** Passive logging plus a weekly digest. The event schema
  leaves room for a rules engine later, but v1 does not evaluate rules on the hot path.
- **No screenshots, no keylogging, no clipboard.**
- **No self-update.** The window says when a newer release exists; the person installs it.

---

## 2. Architecture

Two binaries over a shared, OS-agnostic core.

```
toki-core/          Pure logic. No windows-rs, no COM, no I/O on the hot path.
                    Consumes an abstract FocusEvent stream, emits closed Sessions.
                    Owns: session assembly, presence rules, noise floor, day
                    bucketing, retention rollup, title->domain matching.

toki-store/         SQLite. Schema, interning, the day-clamp query, crash recovery.
                    Shared because the daemon writes it and the GUI reads it; one
                    definition of the schema and the clamp, so they cannot drift.

toki-daemon.exe     Thin Windows adapter + tray + named pipe server.
                    Autostarts at login. Always resident. ~3-5 MB RSS.

toki.exe            Tauri v2 GUI. Launched on demand, exits fully on close.
                    Read-only SQLite. Talks to the daemon over a named pipe.
```

The split exists because a WebView2 host is 50–90 MB resident. Keeping the collector out of
it is the difference between the stated memory target and an Electron-class footprint.

`toki-core` is OS-agnostic so that the logic most likely to contain bugs (idle backdating,
audio presence, the 2-second merge, heartbeat recovery, midnight splitting) is testable as
pure functions, and so a recorded event log can be replayed to reproduce a bad day.

### Daemon threads

| Thread | Work |
|---|---|
| Hook | `SetWinEventHook` callback. Pushes to a channel and returns immediately. |
| Core | Consumes events, applies rules, writes SQLite. Sole writer. |
| Timer | 15s presence check, 30s heartbeat. The resolver runs on triggers (§6). |
| IPC | Named pipe server. |

The hook callback must stay trivial. Windows silently unhooks callbacks that block, and a
tracker that stops recording without saying so is the failure mode this design fears most.

---

## 3. Capture

Event-driven, not polled.

- `EVENT_SYSTEM_FOREGROUND` — the foreground window changed.
- `EVENT_OBJECT_NAMECHANGE`, filtered to the foreground HWND and `OBJID_WINDOW` — the title
  changed, which is how browser tab switches surface. Switching tabs does not change the
  foreground window, so a foreground-only tracker records "Chrome, 3h" and nothing else.

Between events, the foreground is unchanged by definition. A 2-hour fullscreen video
produces one event, no title changes, and one row. Foreground detection never depends on
user input.

### Noise floor

`NAMECHANGE` is chatty: terminals fire per command, video sites fire on title updates.

Buffer the current focus and commit only when it ends. If it lasted under **5 seconds**,
discard it and extend the previous session instead.

Raised from 2s on 2026-09-05, against eight days of capture rather than the estimate this
section originally carried. Two things that measurement changed:

The volume estimate was wrong. It read "roughly 400 rows/day, ~150k rows and ~15 MB per
year before rollup"; actual was ~1,500 rows/day on full days, ~550k rows and ~31 MB/year —
3.7× out. Not that it mattered much: 31 MB/year on a schema that drops titles at six months
is not a cost worth designing around, so row count alone would not have justified moving
the floor.

What did justify it: 30% of all rows were 2–5s focuses carrying 3.6% of tracked time, and
45% of those were a flick away and straight back to the app already in front. The floor
does not discard that time — `ExtendLast` merges it into the preceding session, which for
a flick is the app you were actually working in, so merging is the more truthful record.
The remaining 55% are credited to whatever preceded them, which is the real cost of this
choice and the reason it is a judgement rather than an optimisation.

At 5s, expect ~750 rows/day and ~270k rows per year before rollup.

---

## 4. Presence

Foreground is always known. Presence answers a separate question: are you still there.

Checked every 15s (one `GetLastInputInfo` syscall):

```
idle_ms = now - last_input
if idle_ms >= 5 minutes:
    if foreground process has an active WASAPI render session
       and this audio-only stretch is under 2 hours:
        stay present
    else:
        close the session, end = last_input timestamp
```

**Backdating is the point.** Ending at the moment of detection rather than the last input
adds 5 phantom minutes per break, roughly 40 min/day across 8 breaks.

**5 minutes is still a guess, and stored data cannot settle it.** A pause shorter than the
threshold never closes a session, so its time is counted as active and leaves no trace —
`sessions` keeps start and end, and the silence inside them is gone. Eight days of capture
turned out to say nothing about this for exactly that reason. The daemon therefore keeps a
histogram of absorbed pauses by length, aggregate-only, in a single `meta` row; read it with
`toki-daemon config`. Its cumulative column answers the question directly: everything at or
above a given row would have closed its session instead, had the threshold been that low.

**Audio exemption** exists because Netflix, YouTube and Prime generate zero input for hours.
All hold an active WASAPI render session, including when muted in the player, since the
stream keeps running. Checked via COM `IAudioSessionManager2`, fired only at the instant
idle would otherwise be declared — a few times a day, never on a timer.

The cleaner signal would be the `ES_DISPLAY_REQUIRED` power request, which video players set
and music players don't. Enumerating power requests requires admin (`powercfg /requests`),
so WASAPI is the practical choice.

**The 2-hour cap** covers what WASAPI can't distinguish: music playing to an empty room.
The window is measured from the last input, not from when idle was declared.

On hitting the cap, close with `end = last_input + audio_cap`, crediting the audio-only
stretch up to the cap. Backdating to the last input here would discard the entire film,
which is the opposite of what the exemption exists for. A 90-minute film is fully credited;
a 2.5-hour film loses its last 30 minutes; an overnight playlist yields 2 hours instead of 8.

Sessions record which rule kept them alive (`presence = input | audio_only`) so the audio
rule can be audited when a number looks wrong.

### Sleep, lock, crash

The precise events are `WM_POWERBROADCAST` (`PBT_APMSUSPEND`) and
`WTSRegisterSessionNotification` (lock, unlock, fast user switch). Neither fires on a hard
power-off or a killed process.

**The heartbeat is the actual mechanism.** Write `last_seen` to `meta` every 30s. On startup,
close any dangling session at its last heartbeat. One code path covering suspend, crash,
power loss and force-kill, capping data loss at 30 seconds. The events are precision
improvements layered on top, worth about 30 seconds of code each.

Without this, a machine that sleeps at 23:00 with an editor focused logs 9 hours of use.

---

## 5. Identity

The foreground process is not always the real app. `ApplicationFrameHost.exe` hosts every
UWP/Store app; on this machine it currently owns a window titled "Settings" while the actual
process is `SystemSettings`. Resolve by walking child windows for the hosting process.

Key on full exe path. Resolve display name lazily from the version resource.

Linux has no version resource and no icon inside an executable. Both come from the app's
`.desktop` entry, matched surest first: file name equal to the Wayland app id, then
`StartupWMClass`, then a web app's URL, then the executable. Only the collector sees the app
id, so it stores the entry's `Icon=` in `apps.icon`. The GUI resolves that through the icon
theme per the freedesktop specification, and scales any PNG over 64 px down before the
webview holds it, since a webview keeps every image decoded at full size.

Ship a default, user-editable ignore list: `TextInputHost` (Windows Input Experience),
`SearchHost`, unresolvable `ApplicationFrameHost`, and overlay processes such as
`NVIDIA Overlay`, which take foreground briefly and pollute the data.

### Aliases

One alias mechanism, two key types: apps (`chrome.exe` → "Chrome") and domains
(`x.com` → "X"). Rename only. No merging of several keys into one entity.

The display hierarchy is **browser → domain**. YouTube-in-Chrome and YouTube-in-Zen are
separate leaves under separate parents. Rows already carry both `app_id` and `domain_id`, so
this is a grouping choice in the GUI, not a schema change. A flat `GROUP BY domain` view
over the same rows is available as a secondary view.

---

## 6. Domain resolution

Deferred entirely off the hot path. The daemon never opens a browser file in response to an
event.

```
hot path:   hook -> (ts, exe, title) -> closed interval, resolve_state = pending,
            domain guessed from the title
cold path:  at start, on GUI launch, 90 s after a stretch of browsing ends (if the last
            pass is 2 h old), else every 6 h:
              stream each history's visits, keeping only pending titles
              each session takes the latest matching visit before it ended
              unmatched sessions keep their title guess, or none
```

The window title already identifies the active page in real time and for free. What it lacks
is the domain. The browser's own history records every visit with its title, URL and time.
Join them.

Measured on 193 h of real browser time (2026-10-14, `benchmarks/README.md`): history names
70.3% of it. A pass run 72 h late agrees with one run at once on 99.4% of what it names,
which is why passes can be rare. Simulated on 30 days of real sessions, the triggers above
read history about 8 times a day; the old 5-minute timer did 38-105.

### Sources

| Engine | Browser | File (Zen, Chrome, Helium verified on this machine, 2026-08-27) |
|---|---|---|
| Gecko | Zen | `%APPDATA%\zen\Profiles\<id>\places.sqlite` |
| Gecko | Firefox | `%APPDATA%\Mozilla\Firefox\Profiles\<id>\places.sqlite` |
| Chromium | Chrome | `%LOCALAPPDATA%\Google\Chrome\User Data\<profile>\History` |
| Chromium | Chromium | `%LOCALAPPDATA%\Chromium\User Data\<profile>\History` |
| Chromium | Helium | `%LOCALAPPDATA%\imput\Helium\User Data\<profile>\History` |

Helium is an imput fork of Chromium, so its user data sits under a vendor directory rather
than beside the product name, and it uses the identical `History` schema.

Every profile directory is enumerated rather than assuming one well-known name. Gecko uses
random profile names, and Chrome on this machine has three profiles (`Default`, `Profile 1`,
`Profile 2`) — a hardcoded `Default` would silently drop two thirds of the browsing history.
Visits from every profile of every browser feed one matcher.

The files are read in place, so a pass writes nothing:
- Gecko keeps history in WAL mode, which lets an ordinary read-only connection read a
  consistent snapshot while the browser writes.
- A running Chromium holds `History` under an exclusive lock, so a locked open fails. It
  is opened with `nolock=1` and checked like a seqlock: read only if the file has not been
  written for 2 s, and kept only if its write time and size are unchanged afterwards.
  Exclusive locking does not bump the header's change counter on commit (measured), but
  every commit moves the write time, open handle or not. A rejected read is undone.
- Anything else falls back to copying the file with its journal or WAL into Toki's own
  folder, which the daemon empties at start.

`immutable=1` is not used: it skips the WAL and returns stale rows.

Measured on the development machine, 2026-10-14, five profiles: 0 bytes written per pass
(the copies wrote 43.7 MB), about 70 ms, private memory 3.7 MB at peak.

SQLite's page caches share one budget across a process's connections, counted in pages.
A history with 32 KB pages filled the room the store's 4 KB-page connections left, 7.7 MB
for one file, so the daemon caps SQLite at 1 MB with a soft heap limit.

Use `places.sqlite`, not `recovery.jsonlz4`. The session file hands you the active tab index
directly (measured: 4.0 MB compressed, 4 ms to decompress, 96 tabs, `selected` usable), but
it only knows currently-open tabs. Close a tab and a deferred resolve never finds it.
`places.sqlite` is real history and retains it.

### Title matching

1. Strip the browser suffix. Chrome emits `... - Google Chrome`, Zen emits `... — Zen Browser`.
   Per-engine suffix rules. Strip a leading unread count, `(3) `, on both sides.
2. Normalize `www.` off the domain.
3. Take the latest visit with that title stamped no later than the session's end (plus
   2 minutes for late writes). This reads history as it was when the session happened:
   99.8% agreement with the domains recorded a few minutes after each session, against
   98.9% for "most recent visit ever".

Before any history is read, the title's last segment (`... · GitHub` → `github`) names the
site: from a list of 30 well-known sites, or from the domain history has most often
confirmed for that ending. The guess is shown at once and replaced when history answers;
if history never does, it stays, as `RESOLVE_BY_TITLE`. The list alone names 33.6% of
browser time at 99.8% agreement.

Ambiguity measured on real history:

| | distinct titles | domain-ambiguous after normalization |
|---|---|---|
| Zen | 7,581 | 37 (0.5%) |
| Chrome | 1,393 | 134 (9.6%) |

Chrome's residual is concentrated in auth redirects and payment handoffs
(`"Outlook"` → three hosts, `"X"` → `x.com` and `checkout.stripe.com`) — pages held for
seconds. Time-weighted error is well below the row-count figure, and recency tie-breaking
removes most of what's left.

Exact-URL ambiguity is 28% of distinct titles and is irrelevant here, since of a URL only
the domain is stored.

### Privacy consequence

Incognito and private windows never write to `History` or `places.sqlite`, so their
domains are excluded structurally rather than by a feature that has to work correctly.
This is the main reason the disk-state approach was chosen over UI Automation, which would
read private URLs off the screen and then have to be trusted to discard them.

Their window titles are recorded, but only until the pass that gives up on them: a browser
session history never matched has its title deleted (`RESOLVE_FAILED`, `NOT_A_PAGE` and
`BY_TITLE` rows), so a private tab's caption lasts hours, not six months. Only sessions
history matched keep a title, which is also all the learned site map reads. The cost is
that `reresolve` cannot retry them.

UIA remains a possible opt-in fallback for exact URLs. It is not in v1.

---

## 7. Storage

`%LOCALAPPDATA%\Toki\toki.db`. Never in Documents, OneDrive, or any synced folder — the
database is unencrypted and holds window titles.

SQLite in WAL mode. The daemon is the only writer. The GUI opens read-only.

```sql
CREATE TABLE apps (
  id            INTEGER PRIMARY KEY,
  exe_path      TEXT NOT NULL UNIQUE,
  detected_name TEXT,
  alias         TEXT,
  ignored       INTEGER NOT NULL DEFAULT 0,
  icon          TEXT                     -- Linux: the .desktop entry's Icon= (§5)
);

CREATE TABLE domains (
  id     INTEGER PRIMARY KEY,
  domain TEXT NOT NULL UNIQUE,          -- www-normalized
  alias  TEXT
);

CREATE TABLE titles (
  id    INTEGER PRIMARY KEY,
  title TEXT NOT NULL UNIQUE
);

CREATE TABLE sessions (
  id            INTEGER PRIMARY KEY,
  start_utc_ms  INTEGER NOT NULL,
  end_utc_ms    INTEGER NOT NULL,
  tz_offset_min INTEGER NOT NULL,       -- offset in force when written
  app_id        INTEGER NOT NULL REFERENCES apps(id),
  title_id      INTEGER REFERENCES titles(id),
  domain_id     INTEGER REFERENCES domains(id),
  resolve_state INTEGER NOT NULL DEFAULT 0,  -- 0 pending 1 resolved 2 not-browser 3 failed
  presence      INTEGER NOT NULL            -- 0 input  1 audio-only
);
CREATE INDEX sessions_start   ON sessions(start_utc_ms);
CREATE INDEX sessions_pending ON sessions(resolve_state) WHERE resolve_state = 0;

CREATE TABLE daily_rollup (
  local_date TEXT NOT NULL,             -- YYYY-MM-DD
  app_id     INTEGER NOT NULL,
  domain_id  INTEGER,
  seconds    INTEGER NOT NULL,
  PRIMARY KEY (local_date, app_id, domain_id)
);

CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT);  -- schema_version, last_seen, paused_until
```

Closed intervals rather than an append-only event log: about 400 rows/day, and every query
is a plain `SUM(end - start)` with no window functions.

`sessions` holds *closed* intervals only. The session in progress is mirrored into `meta`
(`open_start`, `open_app`, `open_title`, `open_offset`, `open_engine`) rather than written
as a provisional row. A crash therefore leaves no half-written row to clean up: startup
reads the mirror, closes the session at `last_seen`, and clears it. The alternative — an
open row updated on every heartbeat — would also have to be deleted whenever the noise
floor discarded the session, which is a second way for the store and the tracker to
disagree.

### Time

Store UTC milliseconds plus the UTC offset in force at write time. Day bucketing uses the
offset stored on the row, so past IST days stay IST days after a timezone change, which is
correct — those days really did happen in IST. The offset is read from the system on each
write. No scheduled timezone check.

Day boundary is local midnight.

**Sessions crossing midnight are stored whole and split only for display.** The raw log
stays a faithful record of what happened; the day boundary is a presentation concern.

Clamping a session to a day window is one scalar `min`/`max` pair:

```sql
SELECT app_id, SUM(min(end_utc_ms, :day_end) - max(start_utc_ms, :day_start))
FROM sessions
WHERE start_utc_ms <  :day_end
  AND end_utc_ms   >  :day_start
  AND start_utc_ms >  :day_start - 86400000   -- index bound
GROUP BY app_id;
```

The third predicate exists so this is an index range scan on `sessions_start` rather than a
full table scan; `end_utc_ms > :day_start` alone is not indexable. 24 hours is a safe bound
because a session cannot outlive the 2-hour audio cap plus the idle threshold, and it stays
correct even if those thresholds change. The extra day of scanned rows costs nothing.

The same clamp runs twice when a straddling session is rolled into `daily_rollup`, once per
date.

Display rule: in Today and Week a straddling session contributes its clamped portion to each
day.

### Retention

Raw `sessions` rows are kept 6 months. Older rows collapse into `daily_rollup`: daily totals
per app and per domain, with titles discarded. About 30 rows/day compress to roughly 5.

App and domain identity survives the rollup. Only titles and individual session boundaries
are lost. This preserves the ability to answer "has my YouTube time changed over the year",
which a total-only rollup could not.

### Repair

No delete UI. An accidental click that destroys history is worse than the inconvenience of
opening a terminal.

```
toki purge     --from <date> --to <date> [--app <name>]
toki reresolve --from <date>
```

These exist for non-privacy reasons. A background collector writes bad data silently: a bug
in sleep handling logs 9 hours overnight and permanently skews every average and every chart
axis containing it. During development the domain matcher will write wrong domains for days.
Without a repair path the only fix is dropping the database and losing all history.

---

## 8. IPC

Named pipe at `\\.\pipe\toki-<user SID>`, length-prefixed JSON. Per user, like the
database: another account on the machine runs its own daemon and pipe.

| Message | Purpose |
|---|---|
| `resolve_now` | GUI launch triggers an immediate backfill pass |
| `get_state` / `set_pause` | Read and set pause state |
| `set_history_reading` | Settings → Websites: allow or stop reading browser history |
| (connection succeeds) | Liveness — the GUI can tell the daemon is running |

Liveness matters for a tool whose failure mode is silently recording nothing. Keeping the
GUI read-only preserves the single-writer invariant.

### Pause

Owned by the daemon, since the daemon owns the tray and the GUI exits fully.

Options: 15 minutes, 1 hour, until resumed. The tray icon changes visibly while paused.
Paused time is recorded as nothing at all — not as idle — so it never appears in any total.
Pause auto-resumes on the next daemon start, so a forgotten pause cannot silently kill a
week of data.

---

## 9. GUI

Tauri v2 + Svelte. Hand-rolled charts, no charting library: the views are aggregates over
simple queries, and a day timeline plus a 7×24 heatmap is a few hundred lines of CSS grid
and percentage offsets against 100–300 KB of dependency. They are DOM rather than SVG —
every mark here is a rectangle on one axis, and a `<div>` with a percentage `left` stays
crisp at any window width without a viewBox to keep in sync.

Nothing browser-shaped is shipped: unlike Electron, Tauri calls the WebView2 runtime
Windows already has, which is why `toki.exe` is ~4 MB and the whole installer ~2.2 MB. What
it does cost is a six-process Chromium tree while a window is open, measured at ~90 MB
resident. `bundle.windows.additionalBrowserArgs` trims what can be trimmed —
`--disable-gpu --disable-gpu-compositing` alone drops ~70 MB of committed memory, since a
dashboard of bars and text has nothing for a GPU process to do. Note that setting this key
*replaces* wry's own default rather than appending to it, so the `msWebOOUI,msPdfOOUI,
msSmartScreenProtection` disables have to be repeated there or they are silently lost.

That cost is bounded by the GUI exiting fully on close rather than hiding — the always-
resident footprint is the daemon's ~3 MB, which is the number decision #1 was protecting.

### Views

- **Today** — the day's total against yesterday, longest unbroken stretch, app and browser
  counts, time after midnight; a 24-hour timeline of every session in its real position,
  hatched where the audio exemption held one open; the session in progress; and the per-app
  breakdown, browsers expandable into domains. Steps day by day, with the date in the URL.
- **Week** — total, daily average, busiest day, late-night time; a 7×24 hourly heatmap with
  each day's total at the end of its own row, clickable through to that day on Today; top
  entities and a week-over-week delta. Pages week by week.
- **Apps** — every app ever seen, over 7 days, 30 days or all time: alias editor, ignore
  toggle, share of the range. Searchable across name, alias and executable.
- **Settings** — theme, pause, idle threshold, domain aliases, diagnostics, retention, and
  the paths the data lives at.

Today and Week both distinguish "nothing was recorded" from "this is older than six months",
because `daily_rollup` keeps a day's totals and discards its timing (§7). Settings' Retention
card names the date where that switch happens.

### Visual system

Specified in full in `UI-REVAMP.md` §3, which supersedes the Dub-derived system this section
used to carry. Adapted from Linear's published system rather than a marketing reference,
because this is an instrument someone opens for eleven seconds to answer one question, not a
page that has to sell itself.

The parts worth keeping in front of anyone editing the UI:

| Rule | Reason |
|---|---|
| Dark is the designed-for case; light is the override | An app about late-night usage opening pure white at 11pm is the wrong call. Settings still overrides with System / Light / Dark — following Windows is the right *default*, not the only reasonable answer. The preference has no meaning outside this window, so it lives in the webview's `localStorage`, and an inline script in `app.html` replays it before first paint. |
| Elevation is an inset hairline ring, never a container shadow | `box-shadow: inset 0 0 0 1px` sits in the layout instead of floating above it. Popovers are the exception, since they genuinely do float. |
| Four radii, no fifth: 2 / 6 / 12 / pill | The old 16px large-card radius was used nowhere 12px does not serve. |
| Inter in a 400 / 510 / 590 band, `cv01 ss03 zero`, `tabular-nums` | Nothing reaches 700. Every number here either sits in a column or ticks while the window is open, and proportional digits make both jitter. |
| One chromatic accent per view, spent on the thing being acted on | Selection and "which tab am I on" are answered by a wash plus full-strength ink. On Week the accent is the busiest day; on Apps it is the ignore switch; on Settings it is resuming. |
| A categorical data palette excluding blue | Blue is the accent and would read as "selected". Seven hues, CVD-checked, with an eighth row falling back to neutral rather than inventing a hue. Linear's "no additional chromatic accents" rule governs chrome, not encoding — the primary view is a per-app breakdown needing 6–10 distinguishable colours. Rank order is shared across the ranked list, the timeline and the Apps table, so a colour means "the Nth biggest thing" everywhere. |
| Deltas as an arrow and a magnitude, never red or green | More YouTube this week is not an error condition. |
| Animate `transform` and `opacity` only | `--disable-gpu` is set above, so everything is CPU-rasterised: a `width` or `filter` animation relayouts every frame. `prefers-reduced-motion` removes motion wholesale rather than tuning it down. |
| Input borders stay transparent until hovered or focused | Forty stacked hard borders in the Apps table is loud; the field reads as text until it is pointed at. |

---

## 10. Lifecycle

**Autostart:** `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. No admin required,
appears in Task Manager's Startup tab where it can be disabled to save battery. Not a
Windows service — session 0 cannot see the interactive desktop's foreground window.

**Single instance:** named mutex `Global\TokiDaemon`.

**No watchdog.** The heartbeat caps a crash at 30 seconds of lost data, and `toki doctor`
surfaces the gap.

**Tray:** owned by the daemon via `Shell_NotifyIcon`. Menu: open Toki, pause, quit. Without
it, pausing before something private means killing a process from Task Manager.

**First run:** the installer starts tracking straight away, and its "Launch Toki" checkbox
(on by default) decides whether the GUI opens. The daemon never opens it on its own: it used
to, once, and that overrode an unticked box.

**Weekly digest:** Windows toast covering Monday–Sunday, fired on the first foreground
activity on Monday rather than at a fixed time. With the day boundary at 00:00, a scheduled
digest would fire while asleep or mid-session. Shows total hours and top 5 entities with
week-over-week delta; clicking opens the GUI.

---

## 11. Diagnostics

The realistic failure is the daemon quietly stopping and going unnoticed for a week.

`tracing` to a daily-rotated file in `%LOCALAPPDATA%\Toki\logs\`, 7 days retained, INFO
default.

```
toki doctor
```

Prints daemon uptime, last heartbeat, rows written today, pending unresolved rows, and
whether each browser source file was readable. This is what gets run when the numbers look
wrong.

---

## 12. Packaging

NSIS via the Tauri bundler, published as a GitHub release. Primarily a personal tool;
others using it is fine.

The installer owns the Run key, Start-menu entry and uninstall in one place. Unsigned, which
means one SmartScreen warning at install. No self-update, no code signing unless
distribution ever becomes a real goal.

Update check: once a day, while the window is open, the webview sends one unauthenticated
GET to `api.github.com/repos/Ciatel-Arlanta/Toki/releases/latest` and reads only
`tag_name`. If it is newer than the running version, the rail offers a Download button that
opens the release's stable-named installer (`releases/latest/download/Toki-x64-setup.exe`)
in the browser; the URL is fixed in Rust, not taken from the webview. On by default, said
once at install, off in Settings under Updates. In the window rather than the daemon so the
always-on collector keeps no network code; the cost is that someone who never opens the
window is never told.

---

## 13. Decision log

| # | Decision | Rationale |
|---|---|---|
| 1 | Two binaries + shared core | WebView2 is 50–90 MB; the collector must not live in it |
| 2 | WinEvent hooks, not polling | Tab switches are `NAMECHANGE`, not foreground changes; no idle wakeups |
| 3 | 5-second noise floor | 45% of sub-5s focus is a flick away and back; the preceding session is its truthful home (was 2s, raised on measurement) |
| 4 | Idle 5 min, end backdated to last input | Detection-time ends add ~40 min/day of phantom time |
| 5 | WASAPI audio presence, 2h cap | Films produce no input; `ES_DISPLAY_REQUIRED` needs admin |
| 6 | 30s heartbeat as the sleep/crash mechanism | One path covers suspend, crash, power loss, force-kill |
| 7 | Disk-state domain resolution, deferred | 70 ms per 5 min off the hot path; no browser overhead; incognito structurally excluded |
| 8 | `places.sqlite`, not `recovery.jsonlz4` | Session file loses closed tabs; deferred resolve needs real history |
| 9 | Domain granularity only | 0.5%/9.6% ambiguity vs 28% at exact-URL, and exact URLs aren't wanted |
| 10 | Closed intervals, not an event log | ~400 rows/day, queries are plain SUM |
| 11 | UTC ms + stored offset; sessions stored whole, clamped at query time | Past days keep the offset they happened in; the raw log stays faithful and the clamp is one scalar min/max |
| 12 | 6-month raw retention, then per-app daily rollup | Preserves year-over-year comparison; drops titles |
| 13 | CLI repair, no delete UI | Silent bad data needs a fix; an accidental click does not |
| 14 | Named pipe IPC, GUI read-only | Preserves single-writer; gives liveness detection |
| 15 | OS-agnostic `toki-core` | The buggy logic must be testable without a live desktop |
| 16 | Run key, no watchdog | No admin; heartbeat caps loss at 30s |
| 17 | Svelte + hand-rolled SVG | No virtual DOM, no charting dependency for five simple views |
