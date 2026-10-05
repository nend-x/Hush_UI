/* =========================================================================
   Shared theme application for the table windows.
   The launcher and the main taskbar each have their own copy of this logic;
   the tables share ONE module so a page can never end up with themed
   colors but stale icon-recolor values (the "silver theme, sandcream
   icons" bug): applyTheme() ALWAYS sets the color vars AND the per-theme
   icon-recolor filter values together.
   ========================================================================= */

export interface ThemePayload {
  name: string;
  colors: Record<string, string>;
}

// Per-theme icon recolor hue/sat/brightness. Must match the launcher's
// THEME_ICON_RECOLOR map (computed from each theme's accent color).
export const THEME_ICON_RECOLOR: Record<string, { hue: string; sat: string; brightness: string }> = {
  // Neutral gray theme — recolor filter yields desaturated gray icons
  "material3-dark": { hue: "0deg", sat: "0.0", brightness: "0.95" },
};

export function applyThemeColors(colors: Record<string, string>): void {
  const r = document.documentElement;
  for (const [key, value] of Object.entries(colors)) {
    r.style.setProperty("--" + key, value);
  }
}

/// Apply a theme fully: colors + per-theme icon-recolor values. Every table
/// page must call THIS (not applyThemeColors directly) for both the startup
/// load and live theme://changed events.
export function applyTheme(theme: ThemePayload): void {
  applyThemeColors(theme.colors);
  const recolor = THEME_ICON_RECOLOR[theme.name];
  if (recolor) {
    const r = document.documentElement;
    r.style.setProperty("--icon-recolor-hue", recolor.hue);
    r.style.setProperty("--icon-recolor-sat", recolor.sat);
    r.style.setProperty("--icon-recolor-brightness", recolor.brightness);
  }
}
