<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { goto } from "$app/navigation";
  import { formatDuration, formatDateRange } from "$lib/format";
  import { group, type AppGroup } from "$lib/group";
  import type { DayHours, DayTotal, WeekSummary } from "$lib/week";
  import Heatmap from "$lib/Heatmap.svelte";
  import Metrics from "$lib/Metrics.svelte";
  import RankedList from "$lib/RankedList.svelte";
  import { toolbar } from "$lib/toolbar.svelte";
  import Delta from "$lib/Delta.svelte";
  import ActivitySkeleton from "$lib/ActivitySkeleton.svelte";

  /** 0 is the current week, 1 last week, and so on. */
  let weeksAgo = $state(0);
  let days = $state<DayTotal[]>([]);
  let hours = $state<DayHours[]>([]);
  let groups = $state<AppGroup[]>([]);
  let totalMs = $state(0);
  let deltaMs = $state(0);
  let averageMs = $state(0);
  let lateNightMs = $state(0);
  let hasEarlier = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Refetches whenever the page changes. The previous week's figures stay on screen while
  // the next ones load rather than being cleared: the query is a handful of indexed scans
  // against a local file, so blanking the page would only ever show a flash of nothing.
  $effect(() => {
    void load(weeksAgo);
  });

  async function load(week: number) {
    loading = true;
    try {
      const summary = await invoke<WeekSummary>("week_totals", { weeksAgo: week });
      // Paging faster than the queries return would otherwise let an earlier one land last.
      if (summary.weeksAgo !== weeksAgo) return;
      days = summary.days;
      hours = summary.hours;
      groups = group(summary.top);
      totalMs = summary.totalMs;
      deltaMs = summary.deltaMs;
      averageMs = summary.averageMs;
      lateNightMs = summary.lateNightMs;
      hasEarlier = summary.hasEarlier;
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  function goBack() {
    if (hasEarlier) weeksAgo += 1;
  }

  function goForward() {
    if (weeksAgo > 0) weeksAgo -= 1;
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "ArrowLeft") goBack();
    else if (e.key === "ArrowRight") goForward();
    else return;
    e.preventDefault();
  }

  /** A day in the heatmap opens on Today. Pushed rather than replaced, so Back returns to
      the week it was opened from. */
  function openDay(date: string) {
    void goto(`/?date=${date}`);
  }

  let title = $derived.by(() => {
    if (weeksAgo === 0) return "This week";
    if (weeksAgo === 1) return "Last week";
    if (days.length < 7) return `${weeksAgo} weeks ago`;
    return formatDateRange(days[0].date, days[6].date);
  });

  /** The stepper's own label. Redundant with the eyebrow only for weeks 0 and 1, where the
      eyebrow says "This week" and the reader may still want the dates. */
  let range = $derived(days.length === 7 ? formatDateRange(days[0].date, days[6].date) : "");

  // "vs last week" is only true from the current week. From any other, the comparison is
  // with the week before the one on screen.
  let comparison = $derived(weeksAgo === 0 ? "last week" : "the week before");

  let emptyMessage = $derived(
    weeksAgo === 0 ? "Nothing recorded yet this week." : "Nothing recorded that week.",
  );

  let busiest = $derived.by(() => {
    const recorded = days.filter((d) => !d.isFuture && d.ms > 0);
    if (recorded.length === 0) return null;
    return recorded.reduce((best, d) => (d.ms > best.ms ? d : best));
  });

  // The top bar names the week and carries the week stepper; see `$lib/toolbar.svelte`.
  $effect(() => {
    toolbar.set(title, actions);
    return () => toolbar.clear();
  });

  /** Days that have happened, and how many of them had anything recorded. A week in
      progress counts only its past, so Wednesday's "3 of 3" is not "3 of 7". */
  let countedDays = $derived(days.filter((d) => !d.isFuture).length);
  let activeDays = $derived(days.filter((d) => !d.isFuture && d.ms > 0).length);

  let metrics = $derived([
    { value: formatDuration(averageMs), sub: "Daily average" },
    {
      value: busiest ? busiest.label : "—",
      sub: busiest ? `Busiest day, ${formatDuration(busiest.ms)}` : "No busiest day yet",
    },
    { value: `${activeDays} of ${countedDays}`, sub: "Days with activity" },
    { value: lateNightMs === 0 ? "—" : formatDuration(lateNightMs), sub: "After midnight" },
  ]);
</script>

<svelte:window onkeydown={onKeydown} />

{#snippet actions()}
  <div class="stepper">
    {#if range}<span class="caption">{range}</span>{/if}
    <button
      class="step"
      onclick={goBack}
      disabled={!hasEarlier}
      aria-label="Previous week"
      title="Previous week (left arrow)"><svg viewBox="0 0 10 10" aria-hidden="true"><path d="M6 2L2.5 5.5L6 9" /></svg></button
    >
    <button
      class="step"
      onclick={goForward}
      disabled={weeksAgo === 0}
      aria-label="Next week"
      title="Next week (right arrow)"><svg viewBox="0 0 10 10" aria-hidden="true"><path d="M4 2L7.5 5.5L4 9" /></svg></button
    >
  </div>
{/snippet}

{#if loading && days.length === 0 && !error}
  <ActivitySkeleton week />
{:else}
<header class="page-head">
  <div class="headline">
    <div class="display">{days.length === 0 ? "—" : formatDuration(totalMs)}</div>
    {#if days.length > 0}<Delta ms={deltaMs} against={comparison} />{/if}
  </div>
</header>

{#if !error && days.length > 0}
  <Metrics items={metrics} />

  <section class="card">
    <div class="card-head">
      <h2 class="heading">When</h2>
      <div class="legend" aria-hidden="true">
        less
        <i class="lo"></i><i style="opacity: 0.25"></i><i style="opacity: 0.5"></i><i
          style="opacity: 0.75"
        ></i><i></i>
        more
      </div>
    </div>

    <Heatmap {days} {hours} onpick={openDay} />
  </section>
{/if}

<RankedList {groups} loading={loading && days.length === 0} {error} {emptyMessage} />
{/if}

<style>
  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-3);
  }

  .legend {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    font-size: 11px;
    color: var(--text-faint);
  }

  .legend i {
    width: 12px;
    height: 12px;
    border-radius: var(--radius-badge);
    background: var(--series-1);
  }

  .legend .lo {
    background: var(--surface-raised);
  }
</style>
