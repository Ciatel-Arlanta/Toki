<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { formatDuration } from "$lib/format";
  import { seriesVar } from "$lib/series";
  import { confirmed, setAppAlias, setIgnored } from "$lib/ipc";
  import { toolbar } from "$lib/toolbar.svelte";
  import AppIcon from "$lib/AppIcon.svelte";
  import Skeleton from "$lib/Skeleton.svelte";
  import ListSkeleton from "$lib/ListSkeleton.svelte";

  interface AppEntry {
    exePath: string;
    name: string;
    /** `""` means no alias — normalized from the backend's `null` so `bind:value` works. */
    alias: string;
    ignored: boolean;
    ms: number;
  }

  /** `null` is all time, which the backend resolves to the first date with anything in it. */
  const ranges: { label: string; days: number | null }[] = [
    { label: "7 days", days: 7 },
    { label: "30 days", days: 30 },
    { label: "All time", days: null },
  ];

  let days = $state<number | null>(30);
  let rows = $state<AppEntry[]>([]);
  let loading = $state(true);
  let loaded = $state(false);
  let error = $state<string | null>(null);
  let saveError = $state<string | null>(null);

  let query = $state("");
  let sort = $state<"time" | "name">("time");
  let showIgnored = $state(true);

  let scroller = $state<HTMLElement>();
  let table = $state<HTMLTableElement>();
  let atTop = $state(true);
  let atBottom = $state(true);
  let headerHeight = $state(0);

  function measureScroll() {
    if (!scroller) return;
    if (scroller.scrollHeight - scroller.clientHeight <= 2) {
      atTop = atBottom = true;
    } else {
      atTop = scroller.scrollTop <= 1;
      atBottom = scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 1;
    }
  }

  // The header only changes height when the table resizes, not on every scroll event.
  function measure() {
    measureScroll();
    headerHeight = table?.tHead?.getBoundingClientRect().height ?? 0;
  }

  $effect(() => {
    if (!scroller) return;
    const observer = new ResizeObserver(measure);
    observer.observe(scroller);
    if (table) observer.observe(table);
    measure();
    return () => observer.disconnect();
  });

  $effect(() => {
    void load(days);
  });

  async function load(range: number | null) {
    loading = true;
    try {
      const raw = await invoke<
        { exePath: string; name: string; alias: string | null; ignored: boolean; ms: number }[]
      >("app_directory", { days: range });
      rows = raw.map((r) => ({ ...r, alias: r.alias ?? "" }));
      loaded = true;
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  // Both writes show their result at once and put it back if the daemon refuses, so the
  // table never claims something the database does not hold.
  async function saveAlias(row: AppEntry) {
    const before = { alias: row.alias, name: row.name };
    const alias = row.alias.trim();
    row.alias = alias;
    // The displayed name is `COALESCE(alias, detected_name, exe_path)` in the store, so it
    // has to follow the alias here too rather than wait for the next query.
    row.name = alias || row.exePath;
    saveError = null;
    try {
      await confirmed(setAppAlias(row.exePath, alias || null));
    } catch (e) {
      Object.assign(row, before);
      saveError = String(e);
    }
  }

  async function toggleIgnored(row: AppEntry) {
    row.ignored = !row.ignored;
    saveError = null;
    try {
      await confirmed(setIgnored(row.exePath, row.ignored));
    } catch (e) {
      row.ignored = !row.ignored;
      saveError = String(e);
    }
  }

  /**
   * Colour is by rank over the whole range, ignored rows included, so a colour means the
   * same thing here as on Today: the Nth biggest thing. Hiding ignored rows must not
   * renumber the ones that remain.
   */
  let colors = $derived(
    new Map(
      [...rows].sort((a, b) => b.ms - a.ms).map((r, i) => [r.exePath, seriesVar(i)] as const),
    ),
  );

  let maxMs = $derived(Math.max(1, ...rows.map((r) => r.ms)));
  let hiddenCount = $derived(rows.filter((r) => r.ignored).length);

  let visible = $derived.by(() => {
    const needle = query.trim().toLowerCase();
    const matched = rows.filter((r) => {
      if (!showIgnored && r.ignored) return false;
      if (needle === "") return true;
      // The executable is searched too: it is what someone has when they are looking for
      // the row behind a name they do not recognise.
      return (
        r.name.toLowerCase().includes(needle) ||
        r.exePath.toLowerCase().includes(needle) ||
        r.alias.toLowerCase().includes(needle)
      );
    });
    return matched.sort((a, b) =>
      sort === "time" ? b.ms - a.ms : a.name.localeCompare(b.name),
    );
  });

  onMount(() => {
    // Focus follows the page: typing is the first thing anyone does on a table of 40 apps.
    document.querySelector<HTMLInputElement>(".search input")?.focus();
  });

  // The top bar names the page and carries the range picker; see `$lib/toolbar.svelte`.
  $effect(() => {
    toolbar.set("Apps", actions);
    return () => toolbar.clear();
  });
</script>

{#snippet actions()}
  <div class="seg-group" role="group" aria-label="Range">
    {#each ranges as r (r.label)}
      <button
        type="button"
        class="seg-btn"
        class:on={days === r.days}
        aria-pressed={days === r.days}
        onclick={() => (days = r.days)}
      >
        {r.label}
      </button>
    {/each}
  </div>
{/snippet}

<header class="page-head">
  {#if loading && !loaded}<Skeleton width="170px" height="34px" />{:else}<div class="display">{rows.length} seen</div>{/if}
</header>

{#if saveError}<p class="muted" role="alert">Couldn't save: {saveError}</p>{/if}

<div class="toolbar">
  <div class="search">
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" />
    </svg>
    <input type="search" placeholder="Search apps and aliases…" bind:value={query} />
  </div>
  <button
    class="btn tool"
    onclick={() => (sort = sort === "time" ? "name" : "time")}
    title={sort === "time" ? "Sorted by time. Sort by name" : "Sorted by name. Sort by time"}
    aria-label={sort === "time" ? "Sorted by time, switch to name" : "Sorted by name, switch to time"}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M4.5 3v10M2.5 11l2 2 2-2M11.5 13V3M9.5 5l2-2 2 2" />
    </svg>
    {sort === "time" ? "Time" : "Name"}
  </button>
  <!-- The label names the filter rather than the action, and `aria-pressed` plus the wash
       say whether it is on. A button that reads "Show ignored" while ignored rows are on
       screen has to be re-read every time to work out which way round it is. -->
  <button
    class="btn tool"
    onclick={() => (showIgnored = !showIgnored)}
    aria-pressed={!showIgnored}
    aria-label="Hidden apps: {hiddenCount}"
    disabled={hiddenCount === 0}
    title={showIgnored ? "Hide ignored apps again" : "Show the ignored apps too"}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M1.5 8s2.3-4.5 6.5-4.5S14.5 8 14.5 8 12.2 12.5 8 12.5 1.5 8 1.5 8z" />
      <circle cx="8" cy="8" r="2" />
      <path d="M3 13.5 13 2.5" />
    </svg>
    {hiddenCount}
  </button>
</div>

<section class="card table-card">
  <div class="table-scroll" bind:this={scroller} onscroll={measureScroll}
    class:fade-top={!atTop} class:fade-bottom={!atBottom} style:--header-height="{headerHeight}px">
  {#if error}
    <p class="muted">Couldn't read the database: {error}</p>
  {:else if loading && !loaded}
    <ListSkeleton table />
  {:else if visible.length === 0}
    <p class="muted">{query ? `Nothing matching "${query}".` : "No apps recorded yet."}</p>
  {:else}
    <table bind:this={table}>
      <thead>
        <tr>
          <th>App</th>
          <th style="width: 190px">Alias</th>
          <th style="width: 96px" class="num" class:sorted={sort === "time"}>
            Time{sort === "time" ? " ↓" : ""}
          </th>
          <th style="width: 100px">Share</th>
          <th style="width: 72px" class="num">Ignore</th>
        </tr>
      </thead>
      <tbody>
        {#each visible as row (row.exePath)}
          <tr class:ignored={row.ignored}>
            <td>
              <div class="app-cell">
                <AppIcon
                  exePath={row.exePath}
                  name={row.name}
                  color={colors.get(row.exePath) ?? "var(--series-rest)"}
                  size={26}
                />
                <div class="app-text">
                  <div class="app-name">{row.name}</div>
                  <div class="exe" title={row.exePath}>{row.exePath}</div>
                </div>
              </div>
            </td>
            <td>
              <input
                class="alias"
                type="text"
                placeholder="alias"
                aria-label="Alias for {row.name}"
                bind:value={row.alias}
                onblur={() => saveAlias(row)}
              />
            </td>
            <td class="num">{row.ms === 0 ? "—" : formatDuration(row.ms)}</td>
            <td>
              <span class="mini">
                <span
                  style="
                    width: {(row.ms / maxMs) * 100}%;
                    background: {colors.get(row.exePath) ?? 'var(--series-rest)'};
                  "
                ></span>
              </span>
            </td>
            <td>
              <button
                class="toggle"
                class:on={row.ignored}
                role="switch"
                aria-checked={row.ignored}
                aria-label="Ignore {row.name}"
                title={row.ignored
                  ? "Ignored. Left out of every total"
                  : "Leave this app out of your totals"}
                onclick={() => toggleIgnored(row)}
              >
                <i></i>
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  </div>
</section>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  /* One height for the whole row: the field and the two buttons beside it. */
  .search {
    flex: 1;
    height: 34px;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 0 var(--space-3);
    border-radius: var(--radius-control);
    background: var(--surface);
    box-shadow: var(--ring);
    color: var(--text-faint);
    font-size: 13px;
  }

  .search:focus-within {
    box-shadow: var(--ring-strong);
  }

  .search svg {
    width: 13px;
    height: 13px;
    flex: none;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
  }

  /* The field carries the whole row's chrome, so the input itself has none — otherwise the
     focus ring lands inside a box that is already a box. */
  .search input {
    flex: 1;
    border: none;
    background: none;
    outline: none;
    color: var(--text-primary);
    font: inherit;
    font-size: 13px;
  }

  .search input::placeholder {
    color: var(--text-faint);
  }

  .tool {
    height: 34px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 0 var(--space-3);
  }

  .tool svg {
    width: 14px;
    height: 14px;
    flex: none;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .seg-group {
    display: flex;
    padding: 2px;
    border-radius: var(--radius-control);
    background: var(--surface);
    box-shadow: var(--ring);
  }

  .seg-btn {
    padding: 4px var(--space-3);
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

  /* The card is the scroll container, so the header row stays put over a long directory. It
     scrolls sideways too: at the minimum window width the table is wider than the card, and
     clipping it hid the Ignore column with no way to reach it. */
  .table-card {
    display: flex;
    overflow: hidden;
    min-height: 0;
    flex: 1;
    padding: 0;
    /* A real border, not the ring: the ring is an inset shadow, and the sticky header row
       below has an opaque background that starts at the card's top edge and painted over its
       top line, leaving only the corners. Children cannot paint over a border. */
    box-shadow: none;
    border: 1px solid var(--border);
  }

  .table-scroll {
    flex: 1;
    min-width: 0;
    overflow: auto;
    padding: 0 var(--card-padding) var(--card-padding);
    --ft: 0px;
    --fb: 0px;
    transition: --ft 160ms var(--ease), --fb 160ms var(--ease);
    /* Keep the sticky header opaque; the same 28px fades as RankedList start below it. */
    mask-image: linear-gradient(to bottom, #000 0, #000 var(--header-height),
      transparent var(--header-height), #000 calc(var(--header-height) + var(--ft)),
      #000 calc(100% - var(--fb)), transparent 100%),
      linear-gradient(#000, #000), linear-gradient(#000, #000);
    /* Preserve both 10px scrollbar gutters while masking the row content. */
    mask-size: 100% 100%, 10px 100%, 100% 10px;
    mask-position: left top, right top, left bottom;
    mask-repeat: no-repeat;
  }
  .table-scroll.fade-top { --ft: 28px; }
  .table-scroll.fade-bottom { --fb: 28px; }

  .muted {
    margin: var(--card-padding) 0 0;
    color: var(--text-secondary);
  }

  /* Fixed layout so the columns take the widths in the header and App gets the rest. Sized
     by content instead, one long install path widened App until Alias was a sliver. The
     minimum width is where App still has room for a name; below it the card scrolls. */
  table {
    width: 100%;
    min-width: 720px;
    table-layout: fixed;
    border-collapse: collapse;
  }

  thead th {
    position: sticky;
    z-index: 2;
    top: 0;
    padding: var(--card-padding) var(--space-2) var(--space-2);
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    text-align: left;
    font-size: 11px;
    font-weight: 510;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-faint);
  }

  thead th.num {
    text-align: right;
  }

  /* The sorted column is marked by ink weight, not by the accent — the accent means
     "selected", and a sort order is not a selection. */
  thead th.sorted {
    color: var(--text-secondary);
  }

  tbody td {
    padding: 5px var(--space-2);
    border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
    vertical-align: middle;
  }

  tbody tr:last-child td {
    border-bottom: none;
  }

  tbody tr:hover td {
    background: var(--wash-1);
  }

  .app-cell {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
  }

  .app-text {
    min-width: 0;
  }

  .app-name {
    font-weight: 510;
    letter-spacing: -0.006em;
  }

  .exe {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
    color: var(--text-faint);
  }

  /* Forty stacked hard borders is loud in a dense table, so the field reads as text until
     it is pointed at — the reason DESIGN.md §9 already softened these. */
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

  .num {
    text-align: right;
    white-space: nowrap;
    color: var(--text-secondary);
  }

  .mini {
    display: block;
    width: 84px;
    height: 5px;
    border-radius: var(--radius-badge);
    background: var(--wash-2);
    overflow: hidden;
  }

  .mini span {
    display: block;
    height: 100%;
    border-radius: var(--radius-badge);
  }

  /* A switch, off by default: ignoring an app is the unusual choice, and this is the one
     control on the page with a consequence, so it gets the accent. */
  .toggle {
    position: relative;
    display: block;
    width: 26px;
    height: 15px;
    margin-left: auto;
    padding: 0;
    border: none;
    border-radius: var(--radius-pill);
    background: var(--wash-3);
    cursor: pointer;
    transition: background-color var(--dur-fast) var(--ease);
  }

  .toggle i {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 11px;
    height: 11px;
    border-radius: var(--radius-pill);
    background: var(--text-muted);
    /* `translateX` rather than moving `left`: transform and opacity are the two things that
       do not relayout on a CPU-rasterised webview. */
    transition: transform var(--dur-fast) var(--ease), background-color var(--dur-fast) var(--ease);
  }

  .toggle.on {
    background: var(--accent);
  }

  .toggle.on i {
    transform: translateX(11px);
    background: #ffffff;
  }

  /* Dimmed rather than hidden: an ignored app's time is still real, and seeing what it
     costs is the whole reason to look at this page. */
  tr.ignored .app-name,
  tr.ignored .num {
    color: var(--text-faint);
  }

  tr.ignored :global(.icon),
  tr.ignored .mini {
    opacity: 0.35;
  }
</style>
