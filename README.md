# Toki

A local-first screen-time tracker for Windows, and for Linux under Hyprland. Toki records which application holds the
foreground and, for browsers, which domain, into a private SQLite database on your own
disk. There is no cloud, no account, and no network code anywhere in the daemon.

```
3h 42m today — Zen 1h 20m (github.com, docs.google.com…), Code 1h 05m, Discord 40m…
```

## What it does

- Tracks foreground application time, at second resolution, event-driven rather than
  polled — a tab switch or a two-hour video shows up correctly without waking the CPU on
  a timer.
- For browsers (Zen, Firefox, Chrome, Chromium, and forks like Helium), names the active
  tab's **domain** from its window title at once, then confirms it against the browser's own
  history. Of the URL, only the domain is stored. See
  [What it reads and keeps](#what-it-reads-and-keeps).
- Distinguishes real presence from idle time, with a bounded exemption for audio/video
  playback so a film doesn't get logged as "away."
- Survives sleep, lock, crash, and a hard power-off without losing more than ~30 seconds
  of data, via a heartbeat rather than relying on any single OS notification.
- Ships a small always-on collector (`toki-daemon.exe`, aiming for under 5 MB resident)
  and a separate GUI (`toki.exe`) that's launched on demand and reads the database
  read-only — so the thing that's always running never carries a WebView.

## What it deliberately doesn't do

- No cloud, no telemetry, no accounts, no self-update. The window checks GitHub once a day
  for a newer release and offers a download link; nothing about you is sent, and Settings
  turns it off.
- No exact-URL tracking: of a URL, only the domain is stored.
- No limits, blocking, or nudges. Passive logging only.
- No screenshots, no keylogging, no clipboard access.

See [DESIGN.md](DESIGN.md) for the full design and the reasoning behind these choices.

## What it reads and keeps

Everything stays on your machine; nothing is sent anywhere.

- **Window titles.** Toki stores the title of every window it records, for every app, for
  six months. After that the sessions are collapsed into daily totals and the titles are
  deleted. For a browser the window title is the page title, so this can include things
  like email subjects and document names.
- **Private windows.** Incognito and private windows never write to the browser's history,
  so Toki never learns their domain. Their window titles are kept only until the next
  history read finds no match, then deleted, as is the title of any browser page history
  never matched.
- **Browser history.** When a stretch of browsing ends (you switch away from the browser,
  lock the screen, or go idle), and at most every 2 hours, Toki reads the visits in the
  history database of every profile of each supported browser in your user account. It
  reads them in place, read-only; a copy is made only if the file cannot be read
  consistently, inside Toki's own folder, and deleted afterwards. Only the domain of a page
  whose title matched a recorded window is kept. Full URLs and the rest of your history are
  never written to Toki's database. Other Windows or Linux accounts are never read.
  Settings → Websites turns this off; browser time is then named from window titles alone.
- **Browser favicons.** To draw a site's icon beside its name, the window reads the
  favicon file each browser keeps beside its history (`favicons.sqlite`, or `Favicons`).
  It looks up only the domains it is already showing, keeps the icons in memory and never
  writes them or any page address anywhere, and fetches nothing from the internet. It
  follows the same Settings → Websites switch: off, and no favicon file is opened.
- **App icons on Linux.** The collector notes the icon each app's `.desktop` entry names,
  and the window reads that image from your icon theme or the path the entry gives. Nothing
  is fetched or written.
- **Logs** hold timings, counts and errors, never titles or domains.

## Architecture

```
toki-core/     Pure session-assembly logic. No OS dependency, fully unit tested.
toki-store/    SQLite schema, interning, crash recovery. Shared by the daemon (the only
               writer) and the GUI (read-only), so the two can never disagree about the
               database's shape or location.
toki-daemon/   The collector. Everything OS-specific is under platform/: win/ (WinEvent
               hooks, named pipe, Run key) and linux/ (Hyprland IPC, ext-idle-notify,
               logind, StatusNotifierItem tray, Unix socket, systemd user unit). Always
               running.
toki-gui/      Tauri v2 + SvelteKit GUI. Launched on demand from the tray, exits fully
               on close, opens the database read-only.
```

## Building from source

### Windows

**Prerequisites:**

- Rust, `stable-x86_64-pc-windows-msvc` (via [rustup](https://rustup.rs))
- Visual Studio Build Tools with the "Desktop development with C++" workload (needed by
  `windows-rs` and by `rusqlite`'s bundled SQLite)
- [Bun](https://bun.sh) — used to build the GUI's frontend
- WebView2 Runtime — ships with Windows 10/11 by default, no separate install needed

**The daemon:**

```sh
cargo build --release -p toki-daemon
# binary at target/release/toki-daemon.exe
```

**The GUI:**

```sh
cd crates/toki-gui/ui && bun install && cd ..
./ui/node_modules/.bin/tauri build --no-bundle
# binary at target/release/toki.exe
```

(Run from `crates/toki-gui/`, not `ui/` — that's where `tauri.conf.json` and the Rust
`Cargo.toml` live. `bun x tauri` won't resolve the CLI from here since there's no
`package.json` in this directory; invoking the binary the frontend's `bun install` placed
in `ui/node_modules/.bin/` directly sidesteps that.)

(`--no-bundle` skips producing an installer and just builds the executable — drop it to
also produce an NSIS installer via the Tauri bundler.)

Or build everything at once from the workspace root — this builds every crate, including
`toki-gui`:

```sh
cargo build --workspace --release
```

Don't run `toki.exe` from that build, though, even if `ui/build` already exists from a
previous Tauri build: a plain `cargo build` never enables the `custom-protocol` feature,
so the binary comes out expecting a Vite dev server on `localhost:1420` instead of the
bundled frontend — it opens a window that immediately says "localhost refused to
connect". Only the Tauri CLI command above (or `cargo build ... --features
toki-gui/custom-protocol` for a one-off) produces a `toki.exe` that actually works.

Verify a change didn't break anything:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

### Linux

The collector needs **Hyprland**: Wayland leaves "which window is in front" to each
compositor, and Hyprland's IPC is the one spoken so far. Everything else (idle, sleep and
lock, tray, audio) uses freedesktop standards and works on any distribution. Developed on
Omarchy (Arch).

**Prerequisites:** Rust (stable), [Bun](https://bun.sh), WebKitGTK 4.1 and GTK 3 —
`pacman -S webkit2gtk-4.1 gtk3` on Arch, `apt install libwebkit2gtk-4.1-dev libgtk-3-dev
librsvg2-dev libsoup-3.0-dev` on Debian/Ubuntu. At runtime, `pactl` (PipeWire's Pulse
server provides it) for the audio exemption, and a bar with a tray (Waybar has one).

```sh
cd crates/toki-gui/ui && bun install && cd ..
./ui/node_modules/.bin/tauri build             # deb and rpm under target/release/bundle
./ui/node_modules/.bin/tauri build --no-bundle # or just target/release/{toki,toki-daemon}
```

`scripts/prebuild-gui.sh` runs as part of that and builds the daemon too.

## Running it

### Windows

```sh
toki-daemon.exe run              # start the collector (stays resident; tray icon)
toki-daemon.exe doctor           # health check — heartbeat age, rows today, unresolved
                                  # domains, whether each browser history file is readable
toki-daemon.exe autostart on     # start with Windows (HKCU, no admin needed)
toki-daemon.exe purge --from <YYYY-MM-DD> --to <YYYY-MM-DD> [--app <exe>]
toki-daemon.exe reresolve --from <YYYY-MM-DD> [--to <YYYY-MM-DD>]

toki.exe                         # open the GUI (also reachable from the tray icon)
```

The database lives at `%LOCALAPPDATA%\Toki\toki.db` — never in Documents or any synced
folder, since it's unencrypted and holds window titles. Logs are in
`%LOCALAPPDATA%\Toki\logs\`, rotated daily, 7 days retained.

### Linux

To install for your user from a release tarball (or a checkout after `tauri build`), run
`./install-linux.sh` (`scripts/install-linux.sh` in the repo). It needs no root, puts the
binaries in `~/.local/bin` with a launcher entry, turns on autostart and starts tracking.
Re-run it to upgrade.

Otherwise, the same commands as on Windows, without `.exe`. `toki-daemon autostart on` installs a systemd user
service bound to `graphical-session.target`, which uwsm (Omarchy) and the major desktops
start at login; `off` removes it.

The database is at `~/.local/share/toki/toki.db` (`$XDG_DATA_HOME/toki`), logs beside it
in `logs/`, and the IPC socket at `$XDG_RUNTIME_DIR/toki.sock`.

Apps that share one executable — system Electron apps, Omarchy's Chromium web apps,
AppImages — are told apart by their Wayland app id and named from their `.desktop` file.

## License

[MIT](LICENSE)
