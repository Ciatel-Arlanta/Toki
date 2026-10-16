<script lang="ts">
  import ListSkeleton from "./ListSkeleton.svelte";
  import { onMount } from "svelte";
  import { slide } from "svelte/transition";
  import { formatDuration } from "./format";
  import { seriesVar } from "./series";
  import AppIcon from "./AppIcon.svelte";
  import SiteIcon from "./SiteIcon.svelte";
  import type { AppGroup } from "./group";

  let {
    groups,
    loading = false,
    error = null,
    emptyMessage = "Nothing recorded.",
    onhover,
  }: {
    groups: AppGroup[];
    loading?: boolean;
    error?: string | null;
    emptyMessage?: string;
    /** The app under the pointer or keyboard focus, `null` when it leaves. */
    onhover?: (exePath: string | null) => void;
  } = $props();

  let expanded = $state(new Set<string>());

  /**
   * Bars grow from nothing on first paint. `scaleX` rather than `width` on purpose:
   * `--disable-gpu` is set in tauri.conf.json, so this is CPU-rasterised and a width
   * animation would relayout every frame (UI-REVAMP.md §7).
   */
  let grown = $state(false);

  onMount(() => {
    const id = requestAnimationFrame(() => (grown = true));
    return () => cancelAnimationFrame(id);
  });

  function toggle(exePath: string) {
    const next = new Set(expanded);
    if (next.has(exePath)) next.delete(exePath);
    else next.add(exePath);
    expanded = next;
  }

  /**
   * The row under the pointer, tracked here rather than left to `:hover`. A browser only
   * re-evaluates hover when the mouse moves, so with the wheel turning under a still pointer
   * the highlight, and the strip above that follows it, would stay on the row that *was*
   * there until the next twitch. So the pointer's last position is kept, and on every scroll
   * the element beneath it is looked up again.
   */
  let hot = $state<string | null>(null);
  let pointer: { x: number; y: number } | null = null;
  let frame = 0;

  function setHot(exePath: string | null) {
    if (hot === exePath) return;
    hot = exePath;
    onhover?.(exePath);
  }

  const rowAt = (x: number, y: number): string | null =>
    document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-exe]")?.dataset.exe ?? null;

  function onpointermove(e: PointerEvent) {
    pointer = { x: e.clientX, y: e.clientY };
    setHot(rowAt(e.clientX, e.clientY));
  }

  function onpointerleave() {
    pointer = null;
    setHot(null);
  }

  /** Whether there is more list above and below, for the edge fades. */
  let scroller = $state<HTMLElement>();
  let inner = $state<HTMLElement>();
  let atTop = $state(true);
  let atBottom = $state(true);

  function measure() {
    if (!scroller) return;
    atTop = scroller.scrollTop <= 1;
    atBottom = scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 1;
  }

  function onscroll() {
    measure();
    // One lookup per frame however fast the wheel spins.
    if (!pointer || frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      if (pointer) setHot(rowAt(pointer.x, pointer.y));
    });
  }

  // Height changes without a scroll too: a browser row opening, the window resizing.
  $effect(() => {
    if (!scroller || !inner) return;
    const ro = new ResizeObserver(measure);
    ro.observe(scroller);
    ro.observe(inner);
    measure();
    return () => {
      ro.disconnect();
      cancelAnimationFrame(frame);
      frame = 0;
    };
  });

  /**
   * Biggest first by default, which is the order `group()` hands over. Flipping it only
   * reverses the display: each app keeps its rank, and with it its colour, so a hue means the
   * same thing here as on the Day strip whichever way the list runs.
   */
  let ascending = $state(false);

  let ordered = $derived.by(() => {
    const ranked = groups.map((g, rank) => ({ g, rank }));
    return ascending ? ranked.reverse() : ranked;
  });

  function flip() {
    ascending = !ascending;
    // The other end of the list is a different place, so start at its top.
    if (scroller) scroller.scrollTop = 0;
  }

  let maxMs = $derived(groups.length > 0 ? groups[0].ms : 0);
  let totalMs = $derived(groups.reduce((sum, g) => sum + g.ms, 0));

  /** Rounded to whole percent; anything that rounds to nothing is shown as "<1%" rather
      than "0%", which reads as "not recorded" for something that was. */
  function share(ms: number): string {
    if (totalMs === 0) return "";
    const pct = (ms / totalMs) * 100;
    return pct < 0.5 ? "<1%" : `${Math.round(pct)}%`;
  }
</script>

<section class="card">
  {#if loading}
    <ListSkeleton />
  {:else if error}
    <p class="muted">Couldn't read the database: {error}</p>
  {:else if groups.length === 0}
    <p class="muted">{emptyMessage}</p>
  {:else}
    <div class="head">
      <h2 class="heading">Apps</h2>
      <div class="head-right">
        <button
          type="button"
          class="sort"
          onclick={flip}
          title={ascending ? "Least time first. Show most time first" : "Most time first. Show least time first"}
          aria-label={ascending
            ? "Sorted by time, least first. Switch to most first"
            : "Sorted by time, most first. Switch to least first"}
        >
          <svg class:up={ascending} viewBox="0 0 16 16" aria-hidden="true">
            <path d="M8 3v10M4 9l4 4 4-4" />
          </svg>
          Time
        </button>
        <span class="caption">{groups.length === 1 ? "1 app" : `${groups.length} apps`}</span>
      </div>
    </div>
    <!-- Pointer tracking here is only to keep the hover highlight true while the list scrolls
         under a still pointer; every row is a real button, so keyboard use needs none of it. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="scroll"
      class:fade-top={!atTop}
      class:fade-bottom={!atBottom}
      bind:this={scroller}
      {onscroll}
      {onpointermove}
      {onpointerleave}
    >
    <ul class="rows" bind:this={inner}>
      {#each ordered as { g, rank } (g.exePath)}
        <li data-exe={g.exePath}>
          <button
            class="row"
            class:hot={hot === g.exePath}
            class:expandable={g.domains !== null}
            class:open={expanded.has(g.exePath)}
            aria-expanded={g.domains === null ? undefined : expanded.has(g.exePath)}
            aria-disabled={g.domains === null || undefined}
            onclick={() => g.domains && toggle(g.exePath)}
            onfocus={() => setHot(g.exePath)}
            onblur={() => {
              if (!pointer) setHot(null);
            }}
          >
            <AppIcon exePath={g.exePath} name={g.appName} color={seriesVar(rank)} />
            <span class="name">{g.appName}</span>
            <span class="bar-track">
              <span
                class="bar"
                style="
                  --fill: {grown && maxMs > 0 ? g.ms / maxMs : 0};
                  background: {seriesVar(rank)};
                  transition-delay: {Math.min(rank * 20, 140)}ms;
                "
              ></span>
            </span>
            <span class="share">{share(g.ms)}</span>
            <span class="duration">{formatDuration(g.ms)}</span>
            <span class="chevron" class:open={expanded.has(g.exePath)}>
              {#if g.domains !== null}›{/if}
            </span>
          </button>
          {#if g.domains !== null && expanded.has(g.exePath)}
            <ul class="sub-rows" transition:slide={{ duration: 150 }}>
              {#each g.domains as d (d.domain)}
                <li class="sub-row">
                  <SiteIcon domain={d.domain} />
                  <span class="name">{d.domain}</span>
                  <span class="duration">{formatDuration(d.ms)}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </li>
      {/each}
    </ul>
    </div>
  {/if}
</section>

<style>
  /* Always the last card on its page, so it takes whatever height is left under the cards
     above it, and the list inside is what scrolls: the headline, the figures and the Day
     strip stay where they are while a long directory goes by. `flex: 1 1 0` lets it shrink
     to that space; the minimum is what keeps a short window from squeezing it to nothing,
     and there the page scrolls as a whole instead. */
  section {
    flex: 1 1 0;
    min-height: 240px;
    display: flex;
    flex-direction: column;
  }

  .muted {
    margin: 0;
    color: var(--text-secondary);
  }

  .rows,
  .sub-rows {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  /* The scroll area. Rows reach 8px past their column (a negative margin, for the hover wash),
     which a scroll container would clip, so it is widened by that much and padded back: the
     rows sit where they did, and the scrollbar lands in the card's padding. */
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    margin: 0 calc(var(--space-2) * -1);
    padding: 0 var(--space-2);
    /* Edge fades, only where there is more list to scroll to: nothing at the top until it has
       moved, nothing at the bottom once it has run out. `--ft` and `--fb` are registered
       lengths (app.css), so they ease in and out instead of snapping. */
    --ft: 0px;
    --fb: 0px;
    transition:
      --ft 160ms var(--ease),
      --fb 160ms var(--ease);
    -webkit-mask-image: linear-gradient(
      to bottom,
      transparent 0,
      #000 var(--ft),
      #000 calc(100% - var(--fb)),
      transparent 100%
    ), linear-gradient(#000, #000);
    mask-image: linear-gradient(
      to bottom,
      transparent 0,
      #000 var(--ft),
      #000 calc(100% - var(--fb)),
      transparent 100%
    ), linear-gradient(#000, #000);
    mask-size: 100% 100%, 10px 100%;
    mask-position: left top, right top;
    mask-repeat: no-repeat;
  }

  .scroll.fade-top {
    --ft: 28px;
  }

  .scroll.fade-bottom {
    --fb: 28px;
  }

  .row {
    display: grid;
    grid-template-columns: 28px minmax(96px, 168px) 1fr 44px 72px 14px;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    padding: 6px var(--space-2);
    margin: 0 calc(var(--space-2) * -1);
    border: none;
    border-radius: var(--radius-control);
    background: none;
    font: inherit;
    color: inherit;
    text-align: left;
    cursor: default;
    transition: background-color var(--dur-fast) var(--ease);
  }

  .row.expandable {
    cursor: pointer;
  }

  /* Every row, not only the expandable ones: pointing at one also isolates its app on the
     strip above, and aria-disabled rather than disabled so the pointer events still fire. */
  .row.hot,
  .row:focus-visible,
  .row.open {
    background: var(--wash-2);
  }

  .row.hot .name,
  .row:focus-visible .name {
    color: var(--text-primary);
  }

  .head {
    flex: none;
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: var(--space-3);
  }

  .head-right {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }

  .sort {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    padding: 0 var(--space-2);
    border: none;
    border-radius: var(--radius-control);
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    font-size: 12px;
    font-weight: 510;
    cursor: pointer;
    transition: background-color var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
  }

  .sort:hover {
    background: var(--wash-2);
    color: var(--text-primary);
  }

  .sort svg {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
    transition: transform var(--dur-base) var(--ease);
  }

  /* Pointing up when the smallest comes first. */
  .sort svg.up {
    transform: rotate(180deg);
  }

  .bar-track {
    display: block;
    height: 8px;
    border-radius: var(--radius-badge);
    background: var(--wash-2);
    overflow: hidden;
  }

  .bar {
    display: block;
    height: 100%;
    border-radius: var(--radius-badge);
    transform: scaleX(var(--fill));
    transform-origin: left center;
    transition: transform var(--dur-slow) var(--ease);
  }

  .name {
    font-weight: 510;
    letter-spacing: -0.006em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .share {
    font-size: 12px;
    color: var(--text-faint);
    text-align: right;
  }

  .duration {
    text-align: right;
    color: var(--text-secondary);
    font-variant-numeric: tabular-nums;
  }

  .chevron {
    display: inline-block;
    color: var(--text-faint);
    font-size: 12px;
    text-align: center;
    transition: transform var(--dur-fast) var(--ease);
  }

  .chevron.open {
    transform: rotate(90deg);
  }

  .sub-rows {
    /* Icon column plus its gap, so a domain sits under the app name above it. */
    padding-left: calc(28px + var(--space-3));
    margin-bottom: var(--space-2);
  }

  .sub-row {
    display: grid;
    align-items: center;
    /* The site's icon, then the parent's trailing columns mirrored so a domain duration
       lands directly under the app duration above it. */
    grid-template-columns: 16px 1fr 72px 14px;
    gap: var(--space-3);
    padding: 3px var(--space-2);
    font-size: 13px;
    color: var(--text-muted);
  }

  .sub-row .duration {
    color: var(--text-muted);
  }
</style>
