"""Serve the built GUI with a stubbed Tauri bridge, for screenshots.

The real `toki.exe` needs WebView2, the daemon and a populated database. This serves
`crates/toki-gui/ui/build` over HTTP with SPA fallback and injects a `__TAURI_INTERNALS__`
stub, so the actual Svelte components render against fixed sample data. It is a *layout and
token* check, not a correctness check â€” nothing here touches the real store, and the numbers
below are invented.

    python scripts/preview-gui.py [port]

For real app icons on Windows, run scripts/export-preview-icons.ps1, then set
TOKI_PREVIEW_ICONS to the exported JSON path before starting this server. Missing
executables keep letter tiles. The default uses invented icons for portable previews.

Add `?light` for the light theme, `?hover=N` to hover the Nth list row, `?click=N` to click it (`?click=sort` flips the app list), `?clock=12h` for 12-hour times, `?selftest` to scroll the app list and report hover and fades. Open `/?busy` for a day with hundreds of short sessions (the timeline's worst case).
"""

import http.server
import json
import os
import random
import sys
from datetime import date, timedelta
from pathlib import Path
from urllib.parse import parse_qs

ROOT = Path(__file__).resolve().parent.parent / "crates" / "toki-gui" / "ui" / "build"
ICON_FILE = os.environ.get("TOKI_PREVIEW_ICONS")
NATIVE_ICONS = json.loads(Path(ICON_FILE).read_text(encoding="utf-8-sig")) if ICON_FILE else None
SITE_ICON_FILE = os.environ.get("TOKI_PREVIEW_SITE_ICONS")
SITE_ICONS = json.loads(Path(SITE_ICON_FILE).read_text(encoding="utf-8-sig")) if SITE_ICON_FILE else None


def totals():
    """Shape mirrors `Total` in ui/src/lib/group.ts: one row per (app, domain)."""
    zen = [
        ("github.com", 64),
        ("docs.rs", 38),
        ("youtube.com", 31),
        ("news.ycombinator.com", 18),
        ("localhost", 10),
    ]
    rows = [
        {"exePath": "zen.exe", "appName": "Zen", "domain": d, "ms": m * 60_000}
        for d, m in zen
    ]
    flat = [
        ("Code.exe", "Code", 118),
        ("Discord.exe", "Discord", 34),
        ("WindowsTerminal.exe", "Terminal", 27),
        ("Spotify.exe", "Spotify", 18),
        ("Obsidian.exe", "Obsidian", 9),
        ("explorer.exe", "Explorer", 5),
    ]
    rows += [
        {"exePath": e, "appName": n, "domain": None, "ms": m * 60_000}
        for e, n, m in flat
    ]
    return rows


def week():
    # Monday of the current week, so the "today" bar lands on a real weekday.
    today = date.today()
    monday = today - timedelta(days=today.weekday())
    minutes = [408, 432, 302, 391, 475, 138, 224]
    labels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
    days = []
    for i, (label, m) in enumerate(zip(labels, minutes)):
        d = monday + timedelta(days=i)
        future = d > today
        days.append(
            {
                "date": d.isoformat(),
                "label": label,
                "ms": 0 if future else m * 60_000,
                "isToday": d == today,
                "isFuture": future,
            }
        )
    top = [
        ("zen.exe", "Zen", "github.com", 610),
        ("zen.exe", "Zen", "youtube.com", 372),
        ("Code.exe", "Code", None, 760),
        ("Discord.exe", "Discord", None, 198),
        ("WindowsTerminal.exe", "Terminal", None, 171),
        ("Spotify.exe", "Spotify", None, 124),
        ("Obsidian.exe", "Obsidian", None, 82),
    ]
    recorded = [d for d in days if not d["isFuture"]]
    total_ms = sum(d["ms"] for d in days)
    return {
        "days": days,
        "top": [
            {"exePath": e, "appName": n, "domain": d, "ms": m * 60_000}
            for e, n, d, m in top
        ],
        "totalMs": total_ms,
        "deltaMs": -135 * 60_000,
        "weeksAgo": 0,
        "hasEarlier": True,
        "hours": [
            {"date": d["date"], "hours": hour_shape(d["ms"]), "rolledUp": False}
            for d in days
        ],
        "averageMs": total_ms // max(1, len(recorded)),
        "lateNightMs": 246 * 60_000,
    }


# A plausible waking-day shape, scaled to the day's total. Invented, like everything else
# here: the point is that the heatmap has something with structure to draw.
SHAPE = [
    0.25, 0.10, 0.02, 0, 0, 0, 0, 0.15, 0.50, 0.95, 1.00, 0.90,
    0.35, 0.55, 0.85, 0.95, 0.80, 0.70, 0.30, 0.20, 0.50, 0.65, 0.60, 0.45,
]


def hour_shape(day_ms):
    weight = sum(SHAPE)
    return [round(day_ms * v / weight) for v in SHAPE]


def short_label(d):
    """"Tue 15 Sep" â€” what `short_weekday` plus `short_date` produce on the Rust side."""
    return f"{d.strftime('%a')} {d.day} {d.strftime('%b')}"


def busy_spans():
    """A day of constant switching: hundreds of 10s-4min sessions, for the timeline's worst
    case. Seeded so screenshots are comparable between runs."""
    rng = random.Random(7)
    apps = [
        ("zen.exe", "Zen", 5),
        ("Code.exe", "Code", 4),
        ("Discord.exe", "Discord", 2),
        ("WindowsTerminal.exe", "Terminal", 2),
        ("Spotify.exe", "Spotify", 1),
        ("Obsidian.exe", "Obsidian", 1),
    ]
    apps += [(f"tool{i:02}.exe", f"Tool {i:02}", 0.12) for i in range(40)]
    weights = [w for _, _, w in apps]
    spans, at = [], 7 * 3_600_000 + 40 * 60_000
    while at < 23 * 3_600_000:
        if rng.random() < 0.006:
            at += rng.randint(25, 60) * 60_000  # lunch, a meeting, away
        exe, name, _ = rng.choices(apps, weights)[0]
        length = int(rng.expovariate(1 / 90_000)) + 8_000
        spans.append(
            {
                "startMs": at,
                "endMs": at + length,
                "exePath": exe,
                "appName": name,
                "domain": None,
                "audio": exe == "Spotify.exe" or rng.random() < 0.05,
            }
        )
        at += length + (rng.randint(0, 20_000) if rng.random() < 0.5 else 0)
    return spans


def day_summary(busy=False):
    spans = []
    at = 8 * 3_600_000 + 10 * 60_000
    for exe, name, minutes, audio in [
        ("zen.exe", "Zen", 32, False),
        ("Code.exe", "Code", 75, False),
        ("Discord.exe", "Discord", 23, False),
        ("zen.exe", "Zen", 18, False),
        ("Code.exe", "Code", 50, False),
        ("WindowsTerminal.exe", "Terminal", 25, False),
        ("zen.exe", "Zen", 60, False),
        ("Spotify.exe", "Spotify", 45, True),
        ("Code.exe", "Code", 40, False),
        ("zen.exe", "Zen", 27, True),
    ]:
        at += 11 * 60_000
        spans.append(
            {
                "startMs": at,
                "endMs": at + minutes * 60_000,
                "exePath": exe,
                "appName": name,
                "domain": None,
                "audio": audio,
            }
        )
        at += minutes * 60_000
    rows = totals()
    if busy:
        spans = busy_spans()
        at = spans[-1]["endMs"]
        rows = rows + [
            {"exePath": f"tool{i:02}.exe", "appName": f"Tool {i:02}", "domain": None, "ms": (12 - i // 4) * 60_000}
            for i in range(40)
        ]
    return {
        "date": date.today().isoformat(),
        "label": short_label(date.today()),
        "isToday": True,
        "hasEarlier": True,
        "totals": rows,
        "totalMs": sum(r["ms"] for r in rows),
        "deltaMs": 38 * 60_000,
        "spans": spans,
        "rolledUp": False,
        "longest": {"ms": 75 * 60_000, "exePath": "Code.exe", "appName": "Code", "startMs": 14 * 3_600_000},
        "appCount": 7,
        "browserCount": 1,
        "lateNightMs": 41 * 60_000,
        "now": {
            "exePath": "zen.exe",
            "appName": "Zen",
            "startMs": at - 27 * 60_000,
            "elapsedMs": 14 * 60_000,
        },
    }


APPS = [
    ("zen.exe", "Zen", "Zen", False),
    ("Code.exe", "Code", None, False),
    ("Discord.exe", "Discord", None, False),
    ("WindowsTerminal.exe", "Terminal", "Terminal", False),
    ("Spotify.exe", "Spotify", None, False),
    ("Obsidian.exe", "Obsidian", None, False),
    ("explorer.exe", "Explorer", None, True),
    ("steamwebhelper.exe", "Steam", "Steam", False),
    ("UiPath.Studio.Project", "UiPath", "UiPath", False),
    ("LockApp.exe", "Lock screen", None, True),
]

DOMAINS = [
    ("github.com", "github.com", None),
    ("docs.rs", "docs.rs", None),
    ("youtube.com", "YouTube", "YouTube"),
    ("news.ycombinator.com", "Hacker News", "Hacker News"),
    ("localhost", "localhost", None),
]


def responses(busy=False):
    return {
        "day_summary": day_summary(busy),
        "current_app": day_summary()["now"],
        "week_totals": week(),
        "app_directory": [
            {
                "exePath": e,
                "name": n,
                "alias": a,
                "ignored": g,
                "ms": (40 - i * 4) * 3_600_000,
            }
            for i, (e, n, a, g) in enumerate(APPS)
        ],
        "diagnostics": {
            "appVersion": "0.1.0",
            "schemaVersion": 3,
            "sessionCount": 18402,
            "pendingCount": 14,
            "heartbeatAgeMs": 6_000,
            "daemonPid": 8124,
            "earliestDate": "2026-02-27",
            "detailSince": "2026-03-15",
            "dbBytes": 31_400_000,
            "dbPath": r"C:\Users\you\AppData\Local\Toki\toki.db",
            "logDir": r"C:\Users\you\AppData\Local\Toki\logs",
        },
        "all_apps": [
            {"exePath": e, "name": n, "alias": a, "ignored": g} for e, n, a, g in APPS
        ],
        "all_domains": [{"domain": d, "name": n, "alias": a} for d, n, a in DOMAINS],
        "idle_threshold_minutes": 5,
        "history_reading": True,
        "system_theme": "dark",
        "ipc_call": {"paused": False, "until_ms": None},
    }


def stub(busy=False, light=False):
    return (
        "<script>window.__TAURI_INTERNALS__={invoke:function(c,a){"
        "var r=" + json.dumps(responses(busy)) + ";" + ("r.system_theme='light';" if light else "") + (
        "var nativeIcons=" + json.dumps(NATIVE_ICONS) + ";"
        "if(c==='app_icon'&&nativeIcons!==null)"
        "return Promise.resolve(nativeIcons[a.exePath.toLowerCase()]||null);"
        "var siteIcons=" + json.dumps(SITE_ICONS) + ";"
        "if(c==='domain_icons'&&(siteIcons!==null||nativeIcons!==null)){"
        "var found={};a.domains.forEach(d=>found[d]=(siteIcons||{})[d]||null);"
        "return Promise.resolve(found);}"
        # Invented icons: a rounded square in a hash-derived hue with the app's initial, so
        # the real-icon path can be seen next to the lettered fallback. Two apps answer
        # null on purpose, to keep the fallback on screen.
        "if(c==='app_icon'){var p=a.exePath;"
        "if(/explorer|steamwebhelper/i.test(p))return Promise.resolve(null);"
        "var h=0;for(var i=0;i<p.length;i++)h=(h*31+p.charCodeAt(i))%360;"
        "var svg='<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 32 32\">"
        "<rect width=\"32\" height=\"32\" rx=\"7\" fill=\"hsl('+h+',55%,45%)\"/>"
        "<circle cx=\"16\" cy=\"16\" r=\"7\" fill=\"none\" stroke=\"#fff\" stroke-width=\"3\"/></svg>';"
        "return Promise.resolve('data:image/svg+xml;utf8,'+encodeURIComponent(svg));}"
        # Sites: the same invented icons, except localhost, to keep the neutral tile visible.
        "if(c==='domain_icons'){var o={};a.domains.forEach(function(d){"
        "if(d==='localhost'){o[d]=null;return;}"
        "var h=0;for(var i=0;i<d.length;i++)h=(h*31+d.charCodeAt(i))%360;"
        "var s='<svg xmlns=\\\"http://www.w3.org/2000/svg\\\" viewBox=\\\"0 0 32 32\\\">"
        "<circle cx=\\\"16\\\" cy=\\\"16\\\" r=\\\"15\\\" fill=\\\"hsl('+h+',60%,50%)\\\"/></svg>';"
        "o[d]='data:image/svg+xml;utf8,'+encodeURIComponent(s);});"
        "return Promise.resolve(o);}"
        "return c in r?Promise.resolve(r[c]):Promise.reject('no stub for '+c);}};</script>"
        )
    )


def loading_bridge(enabled: bool) -> str:
    """Hold data requests for a reproducible initial-load preview; no production delays."""
    if not enabled:
        return ""
    return """<script>(()=>{
      const call=window.__TAURI_INTERNALS__.invoke;
      const data=new Set(['day_summary','week_totals','app_directory','diagnostics',
        'idle_threshold_minutes','history_reading','all_domains','ipc_call','current_app']);
      const pending=[];let held=true;
      window.__TAURI_INTERNALS__.invoke=(c,a)=>held&&data.has(c)
        ? new Promise((resolve,reject)=>pending.push(()=>call(c,a).then(resolve,reject)))
        : call(c,a);
      window.finishLoading=()=>{held=false;pending.splice(0).forEach(f=>f());};
    })();</script>"""


def hover_row(n: str) -> str:
    """Fire a pointer-enter on the nth ranked-list row, for screenshotting a hover state
    that headless Chrome has no pointer to produce."""
    if not n.isdigit():
        return ""
    return (
        "<script>window.addEventListener('load',()=>setTimeout(()=>{"
        f"document.querySelectorAll('.row')[{int(n)}]"
        "?.dispatchEvent(new PointerEvent('pointerenter'));},800));</script>"
    )


def click_row(n: str) -> str:
    """Click the nth ranked-list row, to screenshot a browser row expanded, or the list's sort
    button with `sort`."""
    if n == "sort":
        return (
            "<script>window.addEventListener('load',()=>setTimeout(()=>{"
            "document.querySelector('.sort')?.click();},900));</script>"
        )
    if not n.isdigit():
        return ""
    return (
        "<script>window.addEventListener('load',()=>setTimeout(()=>{"
        f"document.querySelectorAll('.row')[{int(n)}]?.click();}},900));</script>"
    )


def clock_pref(v: str) -> str:
    """Set the 12/24-hour preference before the app reads it, as Settings would."""
    if v not in ("12h", "24h"):
        return ""
    return f"<script>try{{localStorage.setItem('toki:clock','{v}')}}catch(e){{}}</script>"


def selftest(v: str) -> str:
    """Park a pointer over the app list, scroll it, and print which row is hot and which edge
    fades are on, so scroll-driven hover can be checked without a real mouse."""
    if v is None:
        return ""
    js = r"""
    window.addEventListener('load', () => setTimeout(() => {
      const show = (t) => {
        const d = document.createElement('div');
        d.style.cssText = 'position:fixed;left:20px;top:80px;z-index:99;background:#fff;color:#000;font:18px monospace;padding:10px;max-width:520px';
        d.textContent = t; document.body.append(d);
      };
      try {
        const sc = document.querySelector('.scroll');
        if (!sc) return show('no .scroll element');
        const r = sc.getBoundingClientRect(), x = r.left + 200, y = r.top + 60;
        sc.dispatchEvent(new PointerEvent('pointermove', {clientX: x, clientY: y, bubbles: true}));
        const hot = () => document.querySelector('.row.hot .name')?.textContent;
        const under = () => document.elementFromPoint(x, y)?.closest('[data-exe]')?.querySelector('.name')?.textContent;
        const first = hot();
        sc.scrollTop = 250;
        setTimeout(() => show(
          'pointer is over: ' + under() + '  |  ' +
          'highlighted: ' + hot() + '  |  ' +
          'match: ' + (under() === hot()) + '  |  ' +
          'fade-top: ' + sc.classList.contains('fade-top') + '  |  ' +
          'fade-bottom: ' + sc.classList.contains('fade-bottom') + '  |  ' +
          'scrollTop: ' + sc.scrollTop), 2500);
      } catch (e) { show('error: ' + e); }
    }, 1500));
    """
    return "<script>" + js + "</script>"


def probe(selectors: str) -> str:
    """Report each selector's box and a few computed styles into a `#probe` element.

    Headless Chrome prints the result with `--dump-dom`, which is the only channel out of a
    headless run. Screenshots alone cannot distinguish "the card clips its rows" from
    "the font loaded late", and reasoning about flexbox from pixels is how the last two false
    alarms happened.
    """
    if not selectors:
        return ""
    return (
        "<script>window.addEventListener('load',()=>setTimeout(()=>{"
        "const out={vh:innerHeight,scrollH:document.documentElement.scrollHeight};"
        f"for(const sel of {json.dumps(selectors.split('|'))}){{"
        "const el=document.querySelector(sel);"
        "if(!el){out[sel]=null;continue;}"
        "const r=el.getBoundingClientRect(),c=getComputedStyle(el);"
        "out[sel]={y:Math.round(r.y),h:Math.round(r.height),bottom:Math.round(r.bottom),"
        "scrollH:el.scrollHeight,flex:c.flex,minH:c.minHeight,overflow:c.overflowY,color:c.color,opacity:c.opacity};}"
        "const pre=document.createElement('pre');pre.id='probe';"
        "pre.textContent=JSON.stringify(out);document.body.append(pre);},1200));</script>"
    )


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=str(ROOT), **kw)

    def do_GET(self):
        raw = self.path.lstrip("/").split("?")
        path = ROOT / raw[0]
        query = parse_qs(raw[1], keep_blank_values=True) if len(raw) > 1 else {}
        # SPA fallback: adapter-static emits one index.html and routes on the client, so
        # /week and /settings have no file of their own.
        if not path.is_file():
            html = (ROOT / "index.html").read_text(encoding="utf-8")
            head = (
                stub("busy" in query, "light" in query)
                + loading_bridge("loading" in query)
                + probe(query.get("probe", [""])[0])
                + hover_row(query.get("hover", [""])[0])
                + click_row(query.get("click", [""])[0])
                + clock_pref(query.get("clock", [""])[0])
                + selftest(query.get("selftest", [None])[0])
            )
            body = html.replace("<head>", "<head>" + head, 1).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 5177
    print(f"serving {ROOT} on http://127.0.0.1:{port}")
    # Threaded: a browser opens several connections at once for the JS chunks, and a
    # single-threaded server leaves the rest of them hanging on the backlog.
    http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
