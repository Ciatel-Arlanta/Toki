import { invoke } from "@tauri-apps/api/core";

/** What the user chose. `system` defers to the desktop's own light/dark setting. */
export type ThemePref = "system" | "light" | "dark";
/** What actually gets painted. */
export type Theme = "light" | "dark";

const PREF_KEY = "toki:theme";
/**
 * The last theme actually applied. Read by the inline script in `app.html` so a launch can
 * paint the right tokens immediately — asking the backend for the system theme is async,
 * while the native window remains hidden until the real preference is resolved.
 */
const RESOLVED_KEY = "toki:theme-resolved";

export function loadPref(): ThemePref {
  try {
    const v = localStorage.getItem(PREF_KEY);
    return v === "light" || v === "dark" || v === "system" ? v : "system";
  } catch {
    return "system";
  }
}

export async function resolveTheme(pref: ThemePref): Promise<Theme> {
  if (pref !== "system") return pref;
  try {
    return (await invoke<string>("system_theme")) === "dark" ? "dark" : "light";
  } catch {
    // Matches the `:root` default, which UI-REVAMP.md §3 flipped to dark. A failure here
    // means the registry read failed, not that the user prefers light.
    return "dark";
  }
}

export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(RESOLVED_KEY, theme);
  } catch {
    // The next launch can still resolve the preference before showing the window.
  }
}

/** Persist a choice and paint it. */
export async function applyPref(pref: ThemePref): Promise<void> {
  try {
    localStorage.setItem(PREF_KEY, pref);
  } catch {
    // Not worth failing the click over; it just won't survive a restart.
  }
  applyTheme(await resolveTheme(pref));
}
