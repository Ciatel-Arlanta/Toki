# Benchmarks

Measured on the author's machine against real data. Rerun after each change to the resolver
and add a row; nothing here is synthetic unless it says so.

- `toki-daemon bench [--hours N] [--label L] [--out results/resolver.jsonl]` times the real
  resolver on a copy of the live database (taken with `VACUUM INTO`; the live file is only
  read). Use a release build.
- `python -I scripts/bench/resolution_study.py --label L` compares naming methods on copies
  of `toki.db` and the browser histories, and writes `results/<date>-<label>.json`.

## Resolver cost per pass

One pass over the last 24 h of browser sessions (`--hours 24`), release build, Windows 11.
Five history sources: Zen, Chrome ×3 profiles, Helium. Memory is sampled every millisecond
during the pass, after trimming the working set, so the copy of the database the bench
works on does not count.

| Version | Written to disk | Time | Private memory (before → peak) | Hit rate |
|---|---|---|---|---|
| step 0: copy every file, index every title | 43.7 MB | 85 ms | 4.1 → 14.1 MB | 83.9% |
| step 1: stream visits, keep pending titles only | 43.7 MB | 95 ms | 4.2 → 12.5 MB | 83.9% |
| step 1b: SQLite soft heap limit 1 MB | 43.7 MB | 100 ms | 2.2 → 3.7 MB | 83.5% |
| step 2: read in place, copy only as a fallback | **0 B** | 74 ms | 2.2 → 3.7 MB | 83.2% |
| step 3: name from titles first | 0 B | 67 ms | 3.2 → 4.1 MB | 83.2% |

On Linux (Omarchy, Hyprland), same pass, release build: three sources, Zen running with its
history in WAL mode under an exclusive lock, Chromium running, and a second Zen profile. The
copy was Zen's 5.2 MB database plus its 2.3 MB WAL, on every pass while the browser ran.

| Version | Written to disk | Time (24 h / all) | Private memory (before → peak) | Hit rate (24 h / all) |
|---|---|---|---|---|
| step 3, `nolock=1`: SQLite refuses WAL, so Zen is copied | 7.5 MB | 18 / 8 ms | 0.6 → 0.9 MB | 100% / 97.9% |
| step 5: `unix-none` VFS, WAL index in memory | **0 B** | 1 / 3 ms | 0.6 → 0.7–1.0 MB | 100% / 97.9% |

Hit rate is history's: resolved over resolvable. It drifts by a few tenths between rows
because each run reads the live data as it is that day. Times vary by about ±30% run to
run; the first run after a while is slower (cold disk cache).

Passes per day (`resolution_study.py`, simulated on the last 30 days of real sessions):

| Schedule | History reads a day | Written a day |
|---|---|---|
| step 0: every 5 min while anything is pending | 38-105 (daemon logs, Oct 7-12) | 1.6-4.4 GB |
| step 4: when browsing ends, at most every 30 min, ceiling 6 h | 20.9 | 0 |
| same, floor 1 h / **2 h (shipped)** / 3 h / 6 h | 12.9 / **7.7** / 5.7 / 3.4 | 0 |

A browsing stretch that ends inside the floor is resolved when the floor runs out, not
dropped until the next trigger.

## How well each method names browser time

193 h of browser sessions (18,918), 26,414 history URLs, 47,347 visits. "Agrees" compares
with the domain the daemon recorded a few minutes after each session.

| Method | Named | Agrees with recorded |
|---|---|---|
| Recorded (resolved ~5 min after the session) | 70.0% | — |
| Every title in history, run today | 70.3% | 98.9% |
| Same, as if run 5 min / 6 h / 72 h after the session | 70.3% | 99.8% / 99.6% / 99.4% |
| Latest visit with the title at or before the session's end | 70.3% | 99.8% |
| Titles only, map of 30 well-known sites | 33.6% | 99.8% |
| Titles only, map learned from history (held-out half) | 42.5% | 98.6% |

How far back the naming visit lies, by share of named time: half within 7 minutes, 90%
within 1.75 h, 95% within 10 h, 99% within 27 days. A short lookback window would lose
names, so the resolver streams all visits and keeps only those whose title is
pending.

Whole-day coverage with titles first (step 3, last 24 h): 56.3% of browser time named,
70.9% over all data. Titles add little to what history names in the end; what they add is
a name the moment a session is written, and a fallback when history reading is off.

## Findings that shaped the design

- Running a pass hours late costs almost nothing in accuracy (99.8% → 99.4% agreement at
  72 h), so passes can be rare.
- Chrome holds its history file under an exclusive lock while it runs. A plain read-only
  open fails; a `nolock=1` open reads it in place in 6 ms.
- Under exclusive locking SQLite does not bump the file change counter on commit (stayed
  at 1 across five commits in a test, and Chrome's live counter did not move in an hour).
  The file's last-write time does change on every commit, with the handle held open, so
  the consistency check for a lock-free read uses write time and size instead.
- Zen (Gecko, WAL) allows an ordinary read-only open while it runs: 14 ms, no copy.
- SQLite's page caches share one budget across a process's connections, counted in pages.
  Scanning Zen's history (32 KB pages) with the store's connections open (4 KB pages) took
  7.7 MB, whatever `cache_size` the history connection had. A 1 MB soft heap limit holds
  it at 1 MB with no change in pass time.
