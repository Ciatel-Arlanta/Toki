<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { formatClock, formatDuration } from "$lib/format";
  import { group, type AppGroup } from "$lib/group";
  import { seriesByExe } from "$lib/series";
  import { shiftDate, todayIso, type DaySummary } from "$lib/day";
  import Metrics from "$lib/Metrics.svelte";
  import RankedList from "$lib/RankedList.svelte";
  import Timeline from "$lib/Timeline.svelte";
  import Delta from "$lib/Delta.svelte";
  import { toolbar } from "$lib/toolbar.svelte";
  import ActivitySkeleton from "$lib/ActivitySkeleton.svelte";

  /**
   * The local date on screen, held in the URL rather than in a variable, so the Week
   * heatmap can open a day by navigating to it and Back returns to the week.
   *
   * Walked by whole days rather than by timestamps: stepping across the hour of a DST
   * change still lands on the day before.
   */
  let date = $derived(page.url.searchParams.get("date") ?? todayIso());

  /** Replaced, not pushed: stepping through a fortnight should not bury the Week view a
      dozen entries deep in history. */
  function show(iso: string) {
    void goto(`/?date=${iso}`, { replaceState: true, noScroll: true, keepFocus: true });
  }
  let summary = $state<DaySummary | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Refetches whenever the date changes. The previous day stays on screen while the next
  // loads rather than being cleared: these are indexed scans against a local file, so
  // blanking the page would only ever show a flash of nothing.
  $effect(() => {
    void load(date);
  });

  async function load(iso: string) {
    loading = true;
    try {
      const next = await invoke<DaySummary>("day_summary", { date: iso });
      // Stepping faster than the queries return would otherwise let an earlier one land last.
      if (next.date !== date) return;
      summary = next;
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  function goBack() {
    if (summary?.hasEarlier) show(shiftDate(date, -1));
  }

  function goForward() {
    if (summary && !summary.isToday) show(shiftDate(date, 1));
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "ArrowLeft") goBack();
    else if (e.key === "ArrowRight") goForward();
    else return;
    e.preventDefault();
  }

  let groups = $derived<AppGroup[]>(summary ? group(summary.totals) : []);
  let colors = $derived(seriesByExe(groups));

  /** The app under the pointer in the list, which the strip isolates. */
  let hovered = $state<string | null>(null);


  let comparison = $derived(summary?.isToday ? "yesterday" : "the day before");

  /** "Today", "Yesterday", or the date itself. The stepper repeats the date underneath only
      when the title has not already said it. */
  let title = $derived.by(() => {
    if (!summary) return "Today";
    if (summary.isToday) return "Today";
    if (summary.date === shiftDate(todayIso(), -1)) return "Yesterday";
    return summary.label;
  });

  /** First start to last end, which is not the same as "was at the computer for" — the gaps
      between are exactly what the strip below is for. */
  let extent = $derived.by(() => {
    const spans = summary?.spans ?? [];
    if (spans.length === 0) return "";
    const from = Math.min(...spans.map((s) => s.startMs));
    const to = Math.max(...spans.map((s) => s.endMs));
    return `${formatClock(from)} – ${formatClock(to)}`;
  });

  // The top bar names the page and carries the date stepper; see `$lib/toolbar.svelte`.
  $effect(() => {
    toolbar.set(title, actions);
    return () => toolbar.clear();
  });

  /** The site that took the most time, across every browser. Sites live under their browser
      in the list below, so this is the one place the strip can say which one led. */
  let topSite = $derived.by(() => {
    const byDomain = new Map<string, number>();
    for (const t of summary?.totals ?? []) {
      if (t.domain) byDomain.set(t.domain, (byDomain.get(t.domain) ?? 0) + t.ms);
    }
    let best: { domain: string; ms: number } | null = null;
    for (const [domain, ms] of byDomain) if (!best || ms > best.ms) best = { domain, ms };
    return best;
  });

  let metrics = $derived.by(() => {
    if (!summary) return [];
    const { longest, browserCount, appCount, lateNightMs } = summary;
    return [
      {
        value: longest ? formatDuration(longest.ms) : "—",
        before: longest ? "Longest in" : undefined,
        sub: longest ? longest.appName : "No unbroken stretch",
        after: longest ? `at ${formatClock(longest.startMs)}` : undefined,
        lead: longest
          ? {
              kind: "app" as const,
              exePath: longest.exePath,
              name: longest.appName,
              color: colors.get(longest.exePath) ?? "var(--series-rest)",
            }
          : undefined,
      },
      {
        value: String(appCount),
        sub: browserCount === 1 ? "Apps, 1 browser" : `Apps, ${browserCount} browsers`,
      },
      {
        value: topSite ? formatDuration(topSite.ms) : "—",
        before: topSite ? "Most time on" : undefined,
        sub: topSite ? topSite.domain : "No site data",
        lead: topSite ? { kind: "site" as const, domain: topSite.domain } : undefined,
      },
      {
        value: lateNightMs === 0 ? "—" : formatDuration(lateNightMs),
        sub: "After midnight",
      },
    ];
  });
</script>

<svelte:window onkeydown={onKeydown} />

{#snippet actions()}
  <div class="stepper">
    {#if summary && title !== summary.label}<span class="caption">{summary.label}</span>{/if}
    <button
      class="step"
      onclick={goBack}
      disabled={!summary?.hasEarlier}
      aria-label="Previous day"
      title="Previous day (left arrow)"><svg viewBox="0 0 10 10" aria-hidden="true"><path d="M6 2L2.5 5.5L6 9" /></svg></button
    >
    <button
      class="step"
      onclick={goForward}
      disabled={!summary || summary.isToday}
      aria-label="Next day"
      title="Next day (right arrow)"><svg viewBox="0 0 10 10" aria-hidden="true"><path d="M4 2L7.5 5.5L4 9" /></svg></button
    >
  </div>
{/snippet}

{#if loading && summary === null && !error}
  <ActivitySkeleton />
{:else}
<header class="page-head">
  <div class="headline">
    <div class="display">{summary ? formatDuration(summary.totalMs) : "—"}</div>
    {#if summary}<Delta ms={summary.deltaMs} against={comparison} />{/if}
  </div>
</header>

{#if summary && !error}
  <Metrics items={metrics} />

  <section class="card">
    <div class="card-head">
      <h2 class="heading">Day</h2>
      {#if extent}<span class="caption">{extent}</span>{/if}
    </div>
    <Timeline spans={summary.spans} {colors} rolledUp={summary.rolledUp} highlight={hovered} />
  </section>
{/if}

<RankedList
  {groups}
  onhover={(exePath) => (hovered = exePath)}
  loading={loading && summary === null}
  {error}
  emptyMessage={summary?.isToday === false
    ? "Nothing recorded on this day."
    : "Nothing recorded yet today."}
/>

{/if}

<style>
  .card-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: var(--space-3);
  }
</style>
