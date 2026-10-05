/* =========================================================================
   TABLE 2 — settings table. The settings menu as a small movable window.
   Drags via the header (data-tauri-drag-region), position persists to
   tables.json backend-side (WindowEvent::Moved throttle).
   New setting types in 0.2: slider, segmented control, text input —
   alongside the launcher's toggles + theme select.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { applyTheme as applyThemeShared, type ThemePayload } from "../shared/theme";
import { initI18n, t } from "../shared/i18n";

type Theme = ThemePayload;

const root = document.getElementById("mt-root")!;

// ===== Show / hide =====
let closing = false;

listen("table://settings-shown", () => {
  closing = false;
  root.classList.remove("shown");
  void root.offsetWidth;
  root.classList.add("shown");
});

function close() {
  if (closing) return;
  closing = true;
  root.classList.remove("shown");
  // Let the pop-out play before the backend hides the window.
  setTimeout(() => invoke("close_table", { name: "settings" }), 240);
}

document.getElementById("mt-close")!.addEventListener("click", close);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") close();
});

// ===== Settings state =====
interface Settings {
  tables_hold_ms?: number;
  clock_24h?: boolean;
  pie_clock?: boolean;
  show_desktop_grid?: boolean;
  language?: string;
}

const WIDGET_IDS = [
  "clipboard-widget",
  "notes-widget",
  "sysmon-widget",
  "audio-widget",
  "brightness-widget",
] as const;

const sliderHold = document.getElementById("slider-hold") as HTMLInputElement;
const holdVal = document.getElementById("hold-val")!;
const themeSelect = document.getElementById("theme-select") as HTMLSelectElement;
const languageSelect = document.getElementById("language-select") as HTMLSelectElement;
const toggleIconRecolor = document.getElementById("toggle-icon-recolor") as HTMLInputElement;
const segClock = document.getElementById("seg-clock")!;
const togglePieClock = document.getElementById("toggle-pie-clock") as HTMLInputElement;
const resetBtn = document.getElementById("mt-reset")!;
const exitBtn = document.getElementById("mt-exit")!;

resetBtn.addEventListener("click", () => {
  resetBtn.classList.add("active");
  invoke("reset_config");
});

exitBtn.addEventListener("click", () => {
  exitBtn.classList.add("active");
  invoke("exit_hush");
});

function currentSettings(): Settings {
  return {
    tables_hold_ms: parseInt(sliderHold.value, 10),
    clock_24h: segClock.querySelector("button.active")?.getAttribute("data-value") === "24",
    pie_clock: togglePieClock.checked,
    language: languageSelect.value,
  };
}

function saveSettings() {
  invoke("save_settings", { settings: currentSettings() });
}

// ===== Load everything =====
async function load() {
  try {
    const s = await invoke<Settings>("load_settings");
    sliderHold.value = String(s.tables_hold_ms ?? 80);
    holdVal.textContent = `${sliderHold.value} ${t("st.ms")}`;
    setSegClock(s.clock_24h ?? true);
    togglePieClock.checked = s.pie_clock ?? true;
    if (s.language === "ru" || s.language === "en") languageSelect.value = s.language;
  } catch {}

  try {
    const visibility = await invoke<Record<string, boolean>>("load_widget_visibility");
    for (const id of WIDGET_IDS) {
      const toggle = document.getElementById(`toggle-${id}`) as HTMLInputElement | null;
      if (toggle) toggle.checked = visibility[id] ?? true;
    }
  } catch {}

  try {
    toggleIconRecolor.checked = await invoke<boolean>("load_icon_recolor");
  } catch {}

  try {
    const theme = await invoke<Theme | null>("get_active_theme");
    if (theme) themeSelect.value = theme.name;
  } catch {}
}

function setSegClock(is24: boolean) {
  segClock.querySelectorAll("button").forEach((b) => {
    b.classList.toggle("active", (b.getAttribute("data-value") === "24") === is24);
  });
}

// ===== Change handlers — save immediately (same as the launcher overlay) =====
sliderHold.addEventListener("input", () => {
  holdVal.textContent = `${sliderHold.value} ${t("st.ms")}`;
  saveSettings();
});

// The language row — save_settings validates + broadcasts settings://changed,
// and this window's own i18n module flips its labels in place with the rest.
languageSelect.addEventListener("change", () => {
  saveSettings();
});

// Re-render dynamic (non data-i18n) strings when the language switches.
listen("i18n:changed", () => {
  holdVal.textContent = `${sliderHold.value} ${t("st.ms")}`;
});

segClock.addEventListener("click", (e) => {
  const btn = (e.target as HTMLElement).closest("button");
  if (!btn) return;
  setSegClock(btn.getAttribute("data-value") === "24");
  saveSettings();
});

for (const id of WIDGET_IDS) {
  const toggle = document.getElementById(`toggle-${id}`) as HTMLInputElement | null;
  toggle?.addEventListener("change", async () => {
    const visibility: Record<string, boolean> = {};
    for (const wid of WIDGET_IDS) {
      const t = document.getElementById(`toggle-${wid}`) as HTMLInputElement | null;
      if (t) visibility[wid] = t.checked;
    }
    try {
      await invoke("save_widget_visibility", { visibility });
      // An open widgets table follows along live.
      void emit("widgets://visibility", visibility);
    } catch {}
  });
}

// ===== Theme (apply + broadcast to every other surface) =====
// Uses the shared applyTheme so colors + icon-recolor values are always
// applied together here too.
function applyTheme(theme: ThemePayload) {
  applyThemeShared(theme);
  // Launcher + taskbar + tables follow.
  void emit("theme://changed", theme);
}

async function loadInitialTheme() {
  try {
    const theme = await invoke<Theme | null>("get_active_theme");
    if (theme) applyTheme(theme);
  } catch {}
}

themeSelect.addEventListener("change", async () => {
  await invoke("set_active_theme", { name: themeSelect.value });
  const theme = await invoke<Theme | null>("get_active_theme").catch(() => null);
  if (theme) applyTheme(theme);
});

// ===== Pie hub clock — persist + broadcast (the pie re-pulls per show too,
// but the broadcast keeps any open surfaces in sync immediately) =====
togglePieClock.addEventListener("change", () => {
  saveSettings();
});

// ===== Icon recolor (apply locally + broadcast — same as launcher) =====
toggleIconRecolor.addEventListener("change", () => {
  document.documentElement.classList.toggle("icon-recolor", toggleIconRecolor.checked);
  invoke("save_icon_recolor", { enabled: toggleIconRecolor.checked });
  void emit("icon-recolor://changed", toggleIconRecolor.checked);
});

// ===== Icon cache maintenance =====
// Search-result icons are extracted via SHGetFileInfoW and cached per path;
// this just drops the cache (they are re-extracted on demand).
const btnClearIcons = document.getElementById("btn-clear-icons") as HTMLButtonElement;

btnClearIcons.addEventListener("click", async () => {
  btnClearIcons.classList.add("active");
  try {
    await invoke("clear_icon_cache");
  } catch {}
  btnClearIcons.textContent = t("st.cleared");
  setTimeout(() => {
    btnClearIcons.classList.remove("active");
    btnClearIcons.textContent = t("st.clear");
  }, 900);
});

// ===== Init =====
(async function init() {
  await initI18n();
  await loadInitialTheme();
  await load();
})();
