<script lang="ts">
  import "../app.css";
  import "@fontsource-variable/inter";
  import { onMount, tick } from "svelte";
  import { invoke, isTauri } from "@tauri-apps/api/core";
  import { page } from "$app/state";
  import { resolveNow } from "$lib/ipc";
  import { applyTheme, loadPref, resolveTheme } from "$lib/theme";
  import RailStatus from "$lib/RailStatus.svelte";
  import WindowControls from "$lib/WindowControls.svelte";
  import { toolbar } from "$lib/toolbar.svelte";
  import UpdateNotice from "$lib/UpdateNotice.svelte";
  import { update } from "$lib/update.svelte";

  let { children } = $props();

  // Only Windows runs frameless (see WindowControls.svelte); everywhere else the compositor draws
  // whatever frame it draws. The rail carries the wordmark either way.
  const customFrame = navigator.userAgent.includes("Windows");

  onMount(async () => {
    const theme = await resolveTheme(loadPref());
    applyTheme(theme);
    // Flush the shell and its theme before revealing the native window. Avoid waiting
    // for animation frames: hidden webviews may suspend those indefinitely.
    await tick();
    if (isTauri()) {
      await invoke("show_main_window", { dark: theme === "dark" }).catch(console.error);
    }

    // Fire-and-forget: DESIGN.md §6 wants an immediate resolver pass on launch, alongside
    // the daemon's own 5-minute timer. Nothing here waits on it — a page that already
    // loaded shows the newly-resolved domains next time it queries, not this instant.
    resolveNow().catch(() => {});
  });

  // Hourly so a window left open for days still notices; `check` itself only asks GitHub
  // once a day. Its own `onMount`, because an async one cannot hand back a cleanup.
  onMount(() => {
    void update.check();
    const id = setInterval(() => void update.check(), 60 * 60 * 1000);
    return () => clearInterval(id);
  });

  // Paths are drawn on a 16x16 grid and inherit `currentColor`, so a tab's icon dims and
  // brightens with its label rather than needing its own selected state.
  const tabs = [
    {
      href: "/",
      label: "Today",
      icon: '<circle cx="8" cy="8" r="6.25" /><path d="M8 4.5V8l2.5 1.5" />',
    },
    { href: "/week", label: "Week", icon: '<path d="M3 13.5V8M8 13.5V2.5M13 13.5V10.5" />' },
    {
      href: "/apps",
      label: "Apps",
      icon:
        '<rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1" />' +
        '<rect x="9" y="2.5" width="4.5" height="4.5" rx="1" />' +
        '<rect x="2.5" y="9" width="4.5" height="4.5" rx="1" />' +
        '<rect x="9" y="9" width="4.5" height="4.5" rx="1" />',
    },
  ];

  // Settings is not a view of the data, so it sits at the foot, above the status, instead of
  // in the list of views.
  const settings = {
    href: "/settings",
    label: "Settings",
    icon: '<path d="M2.5 5h11M2.5 11h11" /><circle cx="6" cy="5" r="1.7" /><circle cx="10" cy="11" r="1.7" />',
  };
</script>

<aside class="rail">
  <!-- The first row of the sidebar, as tall as the title bar beside it so the two share a
       line. A drag region too, since on Windows there is no native caption to grab. -->
  <div class="brand" data-tauri-drag-region={customFrame ? "" : undefined}>
    <img class="mark" src="/toki.svg" alt="" width="20" height="20" />
    <span>Toki</span>
  </div>

  <nav class="nav">
    {#each tabs as tab (tab.href)}
      <a
        href={tab.href}
        class:active={page.url.pathname === tab.href}
        aria-current={page.url.pathname === tab.href ? "page" : undefined}
      >
        <svg viewBox="0 0 16 16" aria-hidden="true">{@html tab.icon}</svg>
        {tab.label}
      </a>
    {/each}
  </nav>

  <div class="rail-end">
    <UpdateNotice />
    <nav class="nav settings">
      <a
        href={settings.href}
        class:active={page.url.pathname === settings.href}
        aria-current={page.url.pathname === settings.href ? "page" : undefined}
      >
        <svg viewBox="0 0 16 16" aria-hidden="true">{@html settings.icon}</svg>
        {settings.label}
      </a>
    </nav>
    <RailStatus />
  </div>
</aside>

<!-- The page's name on the left and its own controls on the right, then the window buttons
     where there is no native frame. Drawn here once; each page only says what goes in it
     (`$lib/toolbar.svelte`). Where the platform draws a frame, the bar is still a toolbar. -->
<header class="topbar" data-tauri-drag-region={customFrame ? "" : undefined}>
  <div class="topbar-title">{toolbar.title ?? [...tabs, settings].find((t) => t.href === page.url.pathname)?.label ?? ""}</div>
  <div class="topbar-right">
    {#if toolbar.actions}
      <div class="actions" class:framed={customFrame}>{@render toolbar.actions()}</div>
    {/if}
    {#if customFrame}<WindowControls />{/if}
  </div>
</header>

<main class="content">
  {@render children()}
</main>

<style>
  /* The shell is a two-column grid on the document body rather than a wrapper element, so
     the rail can reach the full window height without anything in between needing to know
     about it. `:global` because these live outside this component's markup. */
  :global(body) {
    display: grid;
    grid-template-columns: 220px 1fr;
    /* The first row is the top bar, over the content only. The rail spans both rows, so it
       reaches the top edge, and its brand row is the same height as the bar. */
    grid-template-rows: var(--titlebar-h) 1fr;
    height: 100vh;
  }

  :global(:root) {
    --titlebar-h: 40px;
  }

  .content {
    grid-row: 2;
    grid-column: 2;
    min-height: 0;
  }

  .rail {
    grid-row: 1 / -1;
    grid-column: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    /* No bottom padding: the status foot at the end carries its own and sits on the edge. */
    padding: 0 var(--space-3);
    background: var(--rail);
    border-right: 1px solid var(--border);
  }

  /* Pins the update notice and the status footer to the bottom together. */
  .rail-end {
    margin-top: auto;
  }

  /* As tall as the top bar beside it and centred in it, so the wordmark, the page name and
     the window controls all share one line. */
  .brand {
    display: flex;
    align-items: center;
    gap: 6px;
    height: var(--titlebar-h);
    padding: 0 var(--space-2);
    font-size: 14px;
    font-weight: 590;
    letter-spacing: -0.014em;
    user-select: none;
    -webkit-user-select: none;
  }

  /* `data-tauri-drag-region` acts on the element that carries it, so the pieces take no
     pointer events and a press lands on the region itself. */
  .brand > * {
    pointer-events: none;
  }

  .mark {
    display: block;
    flex: none;
  }

  .topbar {
    grid-row: 1;
    grid-column: 2;
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--titlebar-h);
    padding-left: var(--space-8);
    border-bottom: 1px solid var(--border);
    user-select: none;
    -webkit-user-select: none;
  }

  .topbar-title {
    font-size: 13px;
    font-weight: 510;
    letter-spacing: -0.006em;
    color: var(--text-secondary);
    pointer-events: none;
  }

  .topbar-right {
    display: flex;
    align-items: center;
    height: 100%;
  }

  /* Flush with the content's right edge where the bar ends there, and a step clear of the
     window buttons where it does not. */
  .actions {
    display: flex;
    align-items: center;
    margin-right: var(--space-8);
  }

  .actions.framed {
    margin-right: var(--space-3);
  }

  .nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .nav a {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 7px var(--space-2);
    border-radius: var(--radius-control);
    color: var(--text-muted);
    text-decoration: none;
    font-size: 13px;
    font-weight: 510;
    letter-spacing: -0.006em;
    transition: background-color var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
  }

  .nav svg {
    width: 14px;
    height: 14px;
    flex: none;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    opacity: 0.85;
  }

  .nav.settings {
    margin-bottom: var(--space-3);
  }

  .nav a:hover {
    background: var(--wash-1);
    color: var(--text-secondary);
  }

  /* Selected is a wash plus full-strength ink, not the accent. The accent is spent once
     per view on the thing being acted on, and "which page am I on" is answered well enough
     by contrast — see UI-REVAMP.md §2. */
  .nav a.active {
    background: var(--wash-2);
    color: var(--text-primary);
  }

  .content {
    overflow-y: auto;
    padding: var(--gap-section) var(--space-8);
    display: flex;
    flex-direction: column;
    gap: var(--gap-section);
  }
</style>
