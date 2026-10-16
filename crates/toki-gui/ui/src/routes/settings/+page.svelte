<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import {
    getState,
    setPause,
    setDomainAlias,
    setIdleThresholdMs,
    setHistoryReading,
    confirmed,
    type PauseState,
  } from "$lib/ipc";
  import { toolbar } from "$lib/toolbar.svelte";
  import { daemon } from "$lib/daemon.svelte";
  import { update } from "$lib/update.svelte";
  import { applyPref, loadPref, type ThemePref } from "$lib/theme";
  import { clock, clockText, type ClockPref } from "$lib/clock.svelte";
  import { formatAgo, formatBytes, formatCount, formatDay } from "$lib/format";
  import Skeleton from "$lib/Skeleton.svelte";

  interface DomainRow {
    domain: string;
    name: string;
    /** `""` means no alias — normalized from the backend's `null` so `bind:value` works. */
    alias: string;
  }

  interface Diagnostics {
    appVersion: string;
    schemaVersion: number;
    sessionCount: number;
    pendingCount: number;
    heartbeatAgeMs: number | null;
    daemonPid: number | null;
    earliestDate: string | null;
    detailSince: string | null;
    dbBytes: number;
  }

  const MINUTE = 60_000;

  let loading = $state(true);
  let error = $state<string | null>(null);

  let pause = $state<PauseState>({ paused: false, until_ms: null });
  let idleMinutes = $state(5);
  let idleSaved = $state(true);
  let historyReading = $state(true);
  let historyError = $state<string | null>(null);
  let folderError = $state<string | null>(null);
  let idleError = $state<string | null>(null);
  let domainError = $state<string | null>(null);

  async function openFolder(which: "data" | "logs") {
    folderError = null;
    try {
      await invoke("open_folder", { which });
    } catch (e) {
      folderError = String(e);
    }
  }
  let diag = $state<Diagnostics | null>(null);
  let diagnosticsLoading = $state(true);
  let domains = $state<DomainRow[]>([]);

  // Purely a display preference, so it lives in the webview's own storage rather than
  // going through the daemon — nothing outside this window has any use for it.
  let themePref = $state<ThemePref>("system");
  const themeOptions: { value: ThemePref; label: string }[] = [
    { value: "system", label: "System" },
    { value: "light", label: "Light" },
    { value: "dark", label: "Dark" },
  ];

  const clockOptions: { value: ClockPref; label: string }[] = [
    { value: "24h", label: "24-hour" },
    { value: "12h", label: "12-hour" },
  ];

  async function chooseTheme(pref: ThemePref) {
    themePref = pref;
    await applyPref(pref);
  }

  onMount(async () => {
    // Read before the awaits below: it comes from localStorage, so it costs nothing and
    // shouldn't sit behind the daemon round-trip — or a dead daemon would leave the theme
    // control showing the wrong selection.
    themePref = loadPref();

    // Diagnostics reads the database directly, so it survives a daemon that has stopped —
    // which is exactly the state someone opens this page in. Loaded on its own for that
    // reason: either half failing must not blank the other.
    invoke<Diagnostics>("diagnostics")
      .then((d) => (diag = d))
      .catch(() => {})
      .finally(() => (diagnosticsLoading = false));

    try {
      const [state, minutes, reading, rawDomains] = await Promise.all([
        getState(),
        invoke<number>("idle_threshold_minutes"),
        invoke<boolean>("history_reading"),
        invoke<{ domain: string; name: string; alias: string | null }[]>("all_domains"),
      ]);
      adopt(state);
      idleMinutes = minutes;
      historyReading = reading;
      domains = rawDomains.map((d) => ({ ...d, alias: d.alias ?? "" }));
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  /** Every pause answer is authoritative for the rail too, so it goes to both. */
  function adopt(state: PauseState) {
    pause = state;
    daemon.adopt(state);
  }

  async function pauseFor(minutes: number | undefined) {
    adopt(await setPause(true, minutes === undefined ? undefined : minutes * MINUTE));
  }

  async function resume() {
    adopt(await setPause(false));
  }

  // The daemon refuses anything outside 15s-2h, so clamp here rather than let a typo come
  // back as an error the user has to decode. One minute is the smallest whole minute above
  // that floor.
  const IDLE_MIN_MINUTES = 1;
  const IDLE_MAX_MINUTES = 120;

  async function saveIdleThreshold() {
    const minutes = Math.min(
      IDLE_MAX_MINUTES,
      Math.max(IDLE_MIN_MINUTES, Math.round(idleMinutes)),
    );
    idleMinutes = minutes;
    idleError = null;
    try {
      await confirmed(setIdleThresholdMs(minutes * MINUTE));
      idleSaved = true;
    } catch (e) {
      idleError = String(e);
    }
  }

  // Shown as chosen at once, and put back if the daemon refuses: the switch must never
  // claim history reading is off while the daemon still has it on.
  async function chooseHistoryReading(on: boolean) {
    const before = historyReading;
    historyReading = on;
    historyError = null;
    try {
      await confirmed(setHistoryReading(on));
    } catch (e) {
      historyReading = before;
      historyError = String(e);
    }
  }

  async function saveDomainAlias(row: DomainRow) {
    const before = row.alias;
    const alias = row.alias.trim();
    row.alias = alias;
    domainError = null;
    try {
      await confirmed(setDomainAlias(row.domain, alias || null));
    } catch (e) {
      row.alias = before;
      domainError = String(e);
    }
  }

  function formatUntil(ms: number): string {
    const minutes = Math.max(1, Math.round((ms - Date.now()) / MINUTE));
    return `${minutes}m left`;
  }

  let daemonState = $derived.by(() => {
    if (daemon.liveness === "unreachable") return { label: "Not running", ok: false };
    if (daemon.liveness === "checking") return { label: "Checking…", ok: false };
    // The pid is only appended here, where liveness has already been established: nothing
    // clears it on a crash, so on a dead daemon it would name a process that no longer
    // exists.
    const pid = diag?.daemonPid ? ` · pid ${diag.daemonPid}` : "";
    return pause.paused
      ? { label: `Running, paused${pid}`, ok: false }
      : { label: `Running${pid}`, ok: true };
  });

  // The top bar names the page and shows the version; see `$lib/toolbar.svelte`.
  $effect(() => {
    toolbar.set("Settings", actions);
    return () => toolbar.clear();
  });
</script>

{#snippet actions()}
  {#if diag}<span class="tag">v{diag.appVersion}</span>{/if}
{/snippet}

<header class="page-head">
  <div class="display">Toki</div>
</header>

<div class="cols">
  <div class="stack">
    <!-- Outside the loading/error branches below: the theme needs nothing from the daemon,
         so it stays usable on a machine where the daemon has stopped. -->
    <section class="card">
      <h2>Theme</h2>
      <p>System follows your desktop's own light and dark setting.</p>
      <div class="seg-group" role="group" aria-label="Theme">
        {#each themeOptions as opt (opt.value)}
          <button
            type="button"
            class="seg-btn"
            class:on={themePref === opt.value}
            aria-pressed={themePref === opt.value}
            onclick={() => chooseTheme(opt.value)}
          >
            {opt.label}
          </button>
        {/each}
      </div>
    </section>

    <section class="card">
      <h2>Time format</h2>
      <p>
        How times of day are written, on the timeline, the heatmap and the summaries. Now reads
        {clockText(14 * 60 + 30)}.
      </p>
      <div class="seg-group" role="group" aria-label="Time format">
        {#each clockOptions as opt (opt.value)}
          <button
            type="button"
            class="seg-btn"
            class:on={clock.pref === opt.value}
            aria-pressed={clock.pref === opt.value}
            onclick={() => clock.set(opt.value)}
          >
            {opt.label}
          </button>
        {/each}
      </div>
    </section>

    <!-- Also outside the daemon branches: the check runs in this window, not the daemon. -->
    <section class="card">
      <h2>Updates</h2>
      <p>
        Once a day, while this window is open, Toki asks GitHub for the newest release's
        version number. Nothing about you or your data is sent.
      </p>
      <div class="seg-group" role="group" aria-label="Update check">
        {#each [true, false] as on (on)}
          <button
            type="button"
            class="seg-btn"
            class:on={update.enabled === on}
            aria-pressed={update.enabled === on}
            onclick={() => update.setEnabled(on)}
          >
            {on ? "Check daily" : "Never"}
          </button>
        {/each}
      </div>
      {#if update.available}
        <div class="note">Toki {update.available} is available. Download it from the sidebar.</div>
      {/if}
    </section>

    {#if loading}
      <div class="stack" role="status" aria-label="Loading tracking settings" aria-busy="true">
        {#each ["Pause", "Idle threshold", "Websites"] as title}
          <section class="card">
            <h2>{title}</h2>
            <div class="skeleton-copy"><Skeleton width="85%" /></div>
            <Skeleton width="200px" height="30px" round />
          </section>
        {/each}
      </div>
    {:else if error}
      <p class="muted">Couldn't reach the daemon: {error}</p>
    {:else}
      <section class="card">
        <h2>Pause</h2>
        <p>Nothing is recorded while paused. The daemon keeps running.</p>
        {#if pause.paused}
          <div class="banner">
            <span class="dot"></span>
            Paused{pause.until_ms ? `, ${formatUntil(pause.until_ms)}` : " until resumed"}
          </div>
          <div class="btn-row">
            <button class="btn btn-primary" onclick={resume}>Resume tracking</button>
          </div>
        {:else}
          <div class="btn-row">
            <button class="btn" onclick={() => pauseFor(15)}>15 minutes</button>
            <button class="btn" onclick={() => pauseFor(60)}>1 hour</button>
            <button class="btn" onclick={() => pauseFor(undefined)}>Until resumed</button>
          </div>
        {/if}
      </section>

      <section class="card">
        <h2>Idle threshold</h2>
        <p>How long without input before you're counted as away.</p>
        <div class="field-row">
          <input
            class="number"
            type="number"
            aria-label="Idle threshold in minutes"
            min={IDLE_MIN_MINUTES}
            max={IDLE_MAX_MINUTES}
            bind:value={idleMinutes}
            onchange={() => (idleSaved = false)}
          />
          minutes
          <span class="spacer"></span>
          <button class="btn" onclick={saveIdleThreshold} disabled={idleSaved}>
            {idleSaved ? "Saved" : "Save"}
          </button>
        </div>
        {#if idleError}
          <div class="note" role="alert">Couldn't save: {idleError}</div>
        {:else}
          <div class="note">
            Audio keeps a session alive past this for up to 2 hours, so a film is not logged
            as time away.
          </div>
        {/if}
      </section>

      <section class="card">
        <h2>Websites</h2>
        <p>
          Toki names the website in a browser tab from its window title, and confirms it
          against your browsers' history files on this computer, at most every 2 hours.
          Only the domain is kept. Nothing leaves this machine.
        </p>
        <div class="seg-group" role="group" aria-label="How websites are named">
          {#each [true, false] as on (on)}
            <button
              type="button"
              class="seg-btn"
              class:on={historyReading === on}
              aria-pressed={historyReading === on}
              onclick={() => chooseHistoryReading(on)}
            >
              {on ? "Titles and history" : "Titles only"}
            </button>
          {/each}
        </div>
        {#if historyError}
          <div class="note">Couldn't change it: {historyError}</div>
        {:else}
          <div class="note">
            Titles only names about half of browser time, and never opens a browser file.
            With history, about seven in ten.
          </div>
        {/if}
      </section>
    {/if}

    {#if diag}
      <section class="card">
        <h2>Data</h2>
        <p>Everything stays on this machine. Nothing you track is ever sent anywhere.</p>
        <div class="btn-row">
          <button class="btn" onclick={() => openFolder("data")}>Open database folder</button>
          <button class="btn" onclick={() => openFolder("logs")}>Open logs</button>
        </div>
        {#if folderError}<div class="note">{folderError}</div>{/if}
      </section>
    {/if}
  </div>

  <div class="stack">
    {#if diagnosticsLoading}
      <section class="card" role="status" aria-label="Loading diagnostics" aria-busy="true">
        <h2>Diagnostics</h2>
        <p>What the daemon is doing, and whether it is still doing it.</p>
        <div class="diagnostic-loading">
          {#each [1, 2, 3, 4, 5, 6, 7] as row}<Skeleton width="130px" /><Skeleton width="70px" />{/each}
        </div>
      </section>
    {/if}
    {#if diag}
      <section class="card">
        <h2>Diagnostics</h2>
        <p>What the daemon is doing, and whether it is still doing it.</p>
        <dl>
          <dt>Daemon</dt>
          <dd class:ok={daemonState.ok}>{daemonState.label}</dd>
          <dt>Last heartbeat</dt>
          <dd>{diag.heartbeatAgeMs === null ? "never" : formatAgo(diag.heartbeatAgeMs)}</dd>
          <dt>Schema version</dt>
          <dd>{diag.schemaVersion}</dd>
          <dt>Sessions recorded</dt>
          <dd>{formatCount(diag.sessionCount)}</dd>
          <dt>Awaiting domain resolution</dt>
          <dd>{formatCount(diag.pendingCount)}</dd>
          <dt>Earliest day</dt>
          <dd>{diag.earliestDate ? formatDay(diag.earliestDate) : "—"}</dd>
          <dt>Database</dt>
          <dd>{formatBytes(diag.dbBytes)}</dd>
        </dl>
      </section>

      <section class="card">
        <h2>Retention</h2>
        <p>Sessions older than six months collapse into daily totals.</p>
        <dl>
          <dt>Minute-level detail since</dt>
          <dd>{diag.detailSince ? formatDay(diag.detailSince) : "—"}</dd>
          <dt>Daily totals since</dt>
          <dd>{diag.earliestDate ? formatDay(diag.earliestDate) : "—"}</dd>
          <dt>Window titles kept</dt>
          <dd>6 months</dd>
        </dl>
        <div class="note">
          The day timeline and the weekly heatmap need minute-level detail, so they stop at
          the first date above. Older days still have totals. They are not missing, only
          coarser.
        </div>
      </section>
    {/if}

    <!-- Apps moved to their own view, which has the room for a table. Domains stay here:
         there is a handful of them, they are edited once and never looked at again, and a
         second table would be a page about nothing. -->
    {#if !loading && !error && domains.length > 0}
      <section class="card">
        <h2>Domains</h2>
        <p>Rename how a resolved domain shows up.</p>
        {#if domainError}<div class="note" role="alert">Couldn't save: {domainError}</div>{/if}
        <ul class="list">
          {#each domains as row (row.domain)}
            <li class="list-row">
              <span class="path" title={row.domain}>{row.name}</span>
              <input
                class="alias"
                type="text"
                placeholder="alias"
                aria-label="Alias for {row.name}"
                bind:value={row.alias}
                onblur={() => saveDomainAlias(row)}
              />
            </li>
          {/each}
        </ul>
      </section>
    {/if}
  </div>
</div>

<style>
  .skeleton-copy { margin: 12px 0 16px; }
  .diagnostic-loading { display: grid; grid-template-columns: 1fr auto; gap: 18px 16px; margin-top: 16px; }
  .cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
    align-items: start;
  }

  .stack {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-width: 0;
  }

  h2 {
    font-size: 14px;
    font-weight: 510;
    letter-spacing: -0.006em;
  }

  .card p {
    margin: var(--space-1) 0 var(--space-3);
    font-size: 12px;
    color: var(--text-muted);
  }

  .muted {
    margin: 0;
    color: var(--text-muted);
    font-size: 12px;
  }

  .seg-group {
    display: flex;
    padding: 2px;
    border-radius: var(--radius-control);
    background: var(--surface-raised);
    box-shadow: var(--ring);
  }

  /* Segmented control rather than three separate buttons: the options are exclusive, and
     three filled buttons would each read as "this one is active". The selection is marked
     by a wash and full-strength ink rather than the accent — on this page the accent is
     spent on resuming, which is the only action with consequences. */
  .seg-btn {
    flex: 1;
    padding: 5px var(--space-3);
    border: none;
    border-radius: 3px;
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    font-size: 12px;
    font-weight: 510;
    cursor: pointer;
    transition: background-color var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
  }

  .seg-btn:hover:not(.on) {
    color: var(--text-secondary);
  }

  .seg-btn.on {
    background: var(--wash-3);
    color: var(--text-primary);
  }

  .btn-row {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }

  /* Paused is a state the whole window should admit to, so it gets a banner here and the
     amber dot in the rail, not a line of body copy. */
  .banner {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    margin-bottom: var(--space-3);
    border-radius: var(--radius-control);
    background: color-mix(in srgb, var(--series-3) 12%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--series-3) 30%, transparent);
    font-size: 12px;
    font-weight: 510;
    color: var(--text-secondary);
  }

  .banner .dot {
    width: 6px;
    height: 6px;
    flex: none;
    border-radius: var(--radius-pill);
    background: var(--series-3);
  }

  .field-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: 12px;
    color: var(--text-muted);
  }

  .spacer {
    flex: 1;
  }

  .number {
    width: 62px;
    padding: 5px var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    background: var(--surface-raised);
    color: var(--text-primary);
    font: inherit;
    font-size: 13px;
    text-align: right;
  }

  .note {
    margin-top: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-control);
    background: var(--surface-raised);
    font-size: 11px;
    line-height: 1.45;
    color: var(--text-faint);
  }

  dl {
    display: grid;
    grid-template-columns: 1fr auto;
    margin: 0;
  }

  dt,
  dd {
    margin: 0;
    padding: 5px 0;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
    font-size: 12px;
  }

  dt {
    color: var(--text-muted);
  }

  dd {
    text-align: right;
    color: var(--text-secondary);
  }

  /* The last pair closes the list; a rule under it would read as a missing row. */
  dl > :nth-last-child(-n + 2) {
    border-bottom: none;
  }

  /* The one green in the app, and it is a state rather than a category: the daemon either
     is recording or is not. */
  .ok {
    color: var(--series-2);
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 300px;
    overflow-y: auto;
  }

  .list-row {
    display: grid;
    grid-template-columns: 1fr 150px;
    align-items: center;
    gap: var(--space-2);
    padding: 3px 0;
    border-top: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
  }

  .list-row:first-child {
    border-top: none;
  }

  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
  }

  /* The field reads as text until it is pointed at — stacked hard borders are loud in a
     dense form, the reason DESIGN.md §9 already softened these. */
  .alias {
    width: 100%;
    padding: 3px var(--space-2);
    border: 1px solid transparent;
    border-radius: var(--radius-control);
    background: transparent;
    color: var(--text-secondary);
    font: inherit;
    font-size: 13px;
    transition: border-color var(--dur-fast) var(--ease);
  }

  .alias::placeholder {
    color: var(--text-faint);
  }

  .alias:hover,
  .alias:focus {
    border-color: var(--border);
  }
</style>
