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

type Theme = ThemePayload;

const root = document.getElementById("mt-root")!;

// ===== Show / hide =====
let closing = false;

listen("table://settings-shown", () => {
  closing = false;
  root.classList.remove("shown");
  void root.offsetWidth;
  root.classList.add("shown");
  // Index state may have changed since the table was last open.
  void refreshIndexStatus();
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
  show_desktop_grid?: boolean;
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
const toggleIconRecolor = document.getElementById("toggle-icon-recolor") as HTMLInputElement;
const segClock = document.getElementById("seg-clock")!;
const resetBtn = document.getElementById("mt-reset")!;
const exitBtn = document.getElementById("mt-exit")!;

resetBtn.addEventListener("click", () => {
  resetBtn.classList.add("active");
  invoke("reset_config");
});

exitBtn.addEventListener("click", () => {
  exitBtn.classList.add("active");
  invoke("exit_flatui");
});

function currentSettings(): Settings {
  return {
    tables_hold_ms: parseInt(sliderHold.value, 10),
    clock_24h: segClock.querySelector("button.active")?.getAttribute("data-value") === "24",
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
    holdVal.textContent = `${sliderHold.value} ms`;
    setSegClock(s.clock_24h ?? true);
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
  holdVal.textContent = `${sliderHold.value} ms`;
  saveSettings();
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

// ===== Icon recolor (apply locally + broadcast — same as launcher) =====
toggleIconRecolor.addEventListener("change", () => {
  document.documentElement.classList.toggle("icon-recolor", toggleIconRecolor.checked);
  invoke("save_icon_recolor", { enabled: toggleIconRecolor.checked });
  void emit("icon-recolor://changed", toggleIconRecolor.checked);
});

// ===== Search index + caches (0.3.0) =====
//
// The hushlight search walks the Start Menu + Desktop on EVERY keystroke
// unless an index exists. The Index button builds one in the background:
// progress rides on the notification toast (statusbar), the toast hides
// itself 1 s after completion, and the status row refreshes via
// index://progress events.
const indexStatus = document.getElementById("index-status")!;
const btnIndex = document.getElementById("btn-index") as HTMLButtonElement;
const btnClearIndex = document.getElementById("btn-clear-index") as HTMLButtonElement;
const btnClearIcons = document.getElementById("btn-clear-icons") as HTMLButtonElement;

interface IndexStatus {
  indexed: boolean;
  building: boolean;
  count: number;
  built_at: number | null;
}

function describeStatus(s: IndexStatus): string {
  if (s.building) return "Indexing…";
  if (!s.indexed) return "Not indexed";
  const when = s.built_at ? new Date(s.built_at * 1000).toLocaleDateString() : "";
  return `Indexed: ${s.count} items${when ? ` (${when})` : ""}`;
}

async function refreshIndexStatus() {
  try {
    const s = await invoke<IndexStatus>("get_search_index_status");
    indexStatus.textContent = describeStatus(s);
    btnIndex.disabled = s.building;
    btnIndex.textContent = s.building ? "Indexing…" : "Index";
  } catch {}
}

btnIndex.addEventListener("click", () => {
  if (btnIndex.disabled) return;
  btnIndex.disabled = true;
  btnIndex.textContent = "Indexing…";
  indexStatus.textContent = "Indexing…";
  invoke("build_search_index");
});

// The backend emits 0-100 while building; reflect it in the row.
listen<{ progress: number; body?: string }>("index://progress", (e) => {
  if (e.payload.progress < 100) {
    indexStatus.textContent = `Indexing… ${e.payload.progress}%`;
  } else {
    // Build finished — the backend holds the completed toast for 1 s and
    // hides it; pull the fresh "Indexed: N items" status.
    setTimeout(() => void refreshIndexStatus(), 1200);
  }
});

btnClearIndex.addEventListener("click", async () => {
  btnClearIndex.classList.add("active");
  try {
    await invoke("clear_search_cache");
    indexStatus.textContent = "Not indexed";
  } catch {}
  btnClearIndex.textContent = "Cleared";
  setTimeout(() => {
    btnClearIndex.classList.remove("active");
    btnClearIndex.textContent = "Clear";
  }, 900);
});

btnClearIcons.addEventListener("click", async () => {
  btnClearIcons.classList.add("active");
  try {
    await invoke("clear_icon_cache");
  } catch {}
  btnClearIcons.textContent = "Cleared";
  setTimeout(() => {
    btnClearIcons.classList.remove("active");
    btnClearIcons.textContent = "Clear";
  }, 900);
});

// ===== Init =====
(async function init() {
  await loadInitialTheme();
  await load();
  await refreshIndexStatus();
})();
