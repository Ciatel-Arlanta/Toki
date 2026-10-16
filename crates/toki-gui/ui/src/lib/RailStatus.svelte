<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { daemon } from "./daemon.svelte";
  import type { NowPlaying } from "./day";
  import { formatDuration } from "./format";
  import AppIcon from "./AppIcon.svelte";
  import Skeleton from "./Skeleton.svelte";

  /**
   * A timed pause is the one thing on this page that goes stale just by being looked at,
   * so the remaining-minutes label gets a clock. Half a minute is well under the
   * granularity it displays, and the ticker only exists while a timed pause is running —
   * an unpaused window sets no interval at all.
   */
  let now = $state(Date.now());

  let untilMs = $derived(daemon.pause.paused ? daemon.pause.until_ms : null);

  $effect(() => {
    if (untilMs === null) return;
    const until = untilMs;
    const id = setInterval(() => {
      now = Date.now();
      // The daemon unpauses on its own tick; ask, rather than keep showing "1m left".
      if (now >= until) void daemon.refresh();
    }, 30_000);
    return () => clearInterval(id);
  });

  /**
   * The open session, for the "Zen · 14m" line under "Tracking". Polled rather than pushed:
   * the daemon has no event channel to the window, and a minute's staleness on a label
   * quoted in whole minutes is invisible. Paused, there is nothing open to name, so the
   * poll stops rather than repeating a session that has already been closed.
   */
  let current = $state<NowPlaying | null>(null);

  $effect(() => {
    if (daemon.liveness !== "running" || daemon.pause.paused) {
      current = null;
      return;
    }
    const read = () => {
      invoke<NowPlaying | null>("current_app")
        .then((v) => (current = v))
        .catch(() => (current = null));
    };
    read();
    const id = setInterval(read, 60_000);
    return () => clearInterval(id);
  });

  onMount(() => {
    void daemon.refresh();
  });

  let status = $derived.by(() => {
    if (daemon.liveness === "checking") return { tone: "", label: "Checking…", detail: "" };
    if (daemon.liveness === "unreachable") {
      return { tone: "dead", label: "Not running", detail: "Nothing is being recorded" };
    }
    if (!daemon.pause.paused) {
      return {
        tone: "live",
        label: "Tracking",
        // Empty until the first poll answers, and empty again between sessions. The label
        // already carries the state; this line only ever adds detail to it.
        detail: current ? `${current.appName} · ${formatDuration(current.elapsedMs)}` : "",
      };
    }
    return {
      tone: "paused",
      label: "Paused",
      detail: untilMs === null ? "Until resumed" : `${remaining(untilMs, now)} left`,
    };
  });

  function remaining(until: number, from: number): string {
    const minutes = Math.max(1, Math.round((until - from) / 60_000));
    if (minutes < 60) return `${minutes}m`;
    const hours = Math.floor(minutes / 60);
    const rest = minutes % 60;
    return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`;
  }
</script>

<!-- The foot of the sidebar: the one thing in it that says whether the tracker is doing its
     job, set apart from the nav by a rule that runs edge to edge. -->
<div class="status-foot {status.tone}" role="status">
  <div class="label">{status.label}</div>

  {#if daemon.liveness === "checking"}
    <div class="app" aria-hidden="true"><Skeleton width="84px" /><span class="app-name"></span><Skeleton width="28px" /></div>
  {:else if status.tone === "live" && current}
    <div class="app">
      <AppIcon
        exePath={current.exePath}
        name={current.appName}
        color="var(--text-muted)"
        size={20}
        fallback={false}
      />
      <span class="app-name">{current.appName}</span>
      <span class="app-time">{formatDuration(current.elapsedMs)}</span>
    </div>
  {:else if status.detail}
    <div class="detail">{status.detail}</div>
  {/if}
</div>

<style>
  /* Reaches the sidebar's edges: the rail pads its content 12px each side, so the foot
     pulls out by that much and puts it back inside as its own padding. The left padding is
     the nav's (12 + 8), which stands the label on the same line as the nav icons above. */
  .status-foot {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin: 0 calc(var(--space-3) * -1);
    padding: var(--space-3) calc(var(--space-3) + var(--space-2)) var(--space-3);
    border-top: 1px solid var(--border);
  }

  /* Quiet while all is well: the divider and the running app below already say the tracker is
     on. A fault is the exception, so it alone takes a colour. */
  .label {
    font-size: 12px;
    font-weight: 510;
    letter-spacing: -0.006em;
    color: var(--text-muted);
  }

  .paused .label {
    color: var(--series-3);
  }

  .dead .label {
    color: var(--series-7);
  }

  .app {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .app-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    font-weight: 510;
    color: var(--text-secondary);
  }

  .app-time {
    font-size: 12px;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .detail {
    font-size: 12px;
    color: var(--text-muted);
  }
</style>
