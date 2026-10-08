/* =========================================================================
   TABLE 3 — widgets table. The hushlight launcher's widgets in a small
   non-fullscreen movable window (position persists to tables.json).
   Same design language: flat panels, launcher.css widget styles.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { applyTheme, type ThemePayload } from "../shared/theme";
import { initI18n, t } from "../shared/i18n";
import { initNotesWidget } from "../shared/notes-widget";
import { initAudioApps } from "../shared/audio-apps";

const root = document.getElementById("mt-root")!;
const greeting = document.getElementById("mt-greeting")!;
const clipboardList = document.getElementById("clipboard-list")!;
const cpuFill = document.getElementById("cpu-fill")!;
const ramFill = document.getElementById("ram-fill")!;
const cpuVal = document.getElementById("cpu-val")!;
const ramVal = document.getElementById("ram-val")!;
const audioSlider = document.getElementById("audio-master-slider") as HTMLInputElement;
const audioVal = document.getElementById("audio-master-val")!;
const clipboardClearBtn = document.getElementById("clipboard-clear")!;

// ===== Show / hide =====
let closing = false;

listen("table://widgets-shown", () => {
  closing = false;
  root.classList.remove("shown");
  void root.offsetWidth;
  root.classList.add("shown");
  // Fresh data on every open (window positions/toggles may have changed).
  void applyVisibility();
  void updateSysmon();
  void loadAudio();
  void renderGreeting();
  // 0.3.0 idle fix: polling loops (sysmon/CPU/RAM/battery, clipboard)
  // now run ONLY while this table is on screen. The window webview stays
  // alive when hidden, so the old bare setIntervals kept waking the IPC
  // layer forever — the app burned CPU for data nobody could see.
  startPolling();
  fitWindowHeight();
});

function close() {
  if (closing) return;
  closing = true;
  stopPolling();
  root.classList.remove("shown");
  // Let the pop-out play before the backend hides the window.
  setTimeout(() => invoke("close_table", { name: "widgets" }), 240);
}

document.getElementById("mt-close")!.addEventListener("click", close);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") close();
});

// ===== Greeting =====
async function renderGreeting() {
  greeting.textContent = t("wg.hushTable");
}


// ===== Widget visibility (shared config with the launcher + settings table)
const WIDGET_IDS = [
  "clipboard-widget",
  "notes-widget",
  "sysmon-widget",
  "audio-widget",
  "brightness-widget",
] as const;

async function applyVisibility() {
  try {
    const visibility = await invoke<Record<string, boolean>>("load_widget_visibility");
    for (const id of WIDGET_IDS) {
      const el = document.getElementById(id);
      if (el) el.style.display = (visibility[id] ?? true) ? "" : "none";
    }
  } catch {}
}

// table-settings broadcasts changes so an open widgets table updates live.
listen<Record<string, boolean>>("widgets://visibility", (e) => {
  for (const [id, visible] of Object.entries(e.payload)) {
    const el = document.getElementById(id);
    if (el) el.style.display = visible ? "" : "none";
  }
  fitWindowHeight();
});

// ===== Adaptive window height =====
// The window must fit every stacked widget without a column scrollbar
// (clipboard keeps its own 130px list cap and stays the one scrollable
// widget). The frontend measures the natural content height and the
// backend applies it via resize_table_window — so the table also stays
// correct while the per-app volume dropdown is open or collapsed. The
// height change itself doesn't snap: it's stepped through a rAF loop
// with an ease-out curve, so growing/shrinking reads as one motion.
const widgetsCol = document.querySelector<HTMLElement>(".mt-widgets")!;
const mtHeaderEl = document.querySelector<HTMLElement>(".mt-header")!;

// Sum the column's children at their natural sizes. Deliberately NOT
// scrollHeight: once the window is too big, scrollHeight clamps to the
// client box and the window could never shrink back. Child heights
// depend on content only (the width is fixed), so window resizes can't
// re-trigger the observer — no feedback loop.
function naturalColumnHeight(): number {
  const kids = Array.from(widgetsCol.children) as HTMLElement[];
  let sum = 0;
  for (const k of kids) sum += k.offsetHeight; // display:none kids count 0
  const cs = getComputedStyle(widgetsCol);
  const gap = parseFloat(cs.rowGap) || 0;
  sum += gap * Math.max(0, kids.length - 1);
  sum += (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0);
  return sum;
}

let fitTimer: number | null = null;
function fitWindowHeight() {
  if (fitTimer !== null) window.clearTimeout(fitTimer);
  fitTimer = window.setTimeout(() => {
    fitTimer = null;
    if (document.visibilityState !== "visible") return;
    const needed = Math.ceil(
      mtHeaderEl.offsetHeight + naturalColumnHeight() + 2 /* .mt-root borders */,
    );
    animateWindowHeight(needed);
  }, 80);
}

// ===== Smooth resize animation =====
// An OS window can't CSS-transition its own bounds, so the resize is
// stepped: one set_size per animation frame with an ease-out curve
// (~14 frames over 240ms — same motion family as the dropdown's 200ms).
// A fit that changes mid-flight (dropdown retoggle, clipboard churn,
// visibility switches) re-anchors on the last height actually commanded,
// so the motion continues from where the window really is instead of
// snapping. The backend clamps to the monitor ceiling — clamped frames
// simply don't move — and scrollbar-gutter: stable (tables.css) keeps
// the content width constant through every state, so a sliding scrollbar
// can't rewrap the widgets and re-trigger the measurement mid-motion.
const RESIZE_ANIM_MS = 240;
let lastSentH: number | null = null; // last height actually commanded
let resizeRaf: number | null = null;

function animateWindowHeight(targetH: number) {
  if (resizeRaf !== null) {
    cancelAnimationFrame(resizeRaf);
    resizeRaf = null;
  }
  const from = lastSentH ?? Math.round(window.innerHeight);
  lastSentH = from;
  if (Math.abs(targetH - from) < 1) return;
  const t0 = performance.now();
  const easeOut = (t: number) => 1 - Math.pow(1 - t, 3);
  const step = (now: number) => {
    const t = Math.min(1, (now - t0) / RESIZE_ANIM_MS);
    const h = Math.round(from + (targetH - from) * easeOut(t));
    if (h !== lastSentH) {
      lastSentH = h;
      void invoke("resize_table_window", {
        name: "widgets",
        width: window.innerWidth,
        height: h,
      }).catch(() => {});
    }
    if (t < 1) {
      resizeRaf = requestAnimationFrame(step);
    } else {
      resizeRaf = null;
    }
  };
  resizeRaf = requestAnimationFrame(step);
}

// Anything that changes widget heights lands here: the dropdown's
// open/close animation, clipboard items, note buttons, theme font
// rewraps, i18n switches. The 80ms debounce rides out animation frames.
const columnRO = new ResizeObserver(() => fitWindowHeight());
for (const k of Array.from(widgetsCol.children)) columnRO.observe(k);

// Belt and suspenders: if table://widgets-shown ever arrives before the
// OS window is actually visible, the fit's visibility guard skips it —
// refit the moment the window does become visible.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") fitWindowHeight();
});

// ===== Polling lifecycle (0.3.0 idle fix) =====
// Timers exist only between table://widgets-shown and close(). While the
// table is hidden the webview is fully idle — zero IPC, zero wakeups.
let sysmonTimer: number | null = null;
let clipboardTimer: number | null = null;

function startPolling() {
  stopPolling();
  sysmonTimer = window.setInterval(updateSysmon, 2000);
  clipboardTimer = window.setInterval(pollClipboard, 500);
}

function stopPolling() {
  if (sysmonTimer !== null) {
    window.clearInterval(sysmonTimer);
    sysmonTimer = null;
  }
  if (clipboardTimer !== null) {
    window.clearInterval(clipboardTimer);
    clipboardTimer = null;
  }
}

// ===== Sysmon widget (visibility-gated — same idle-hang guard as the launcher)
async function updateSysmon() {
  if (document.visibilityState !== "visible") return;
  if (document.getElementById("sysmon-widget")?.style.display === "none") return;
  try {
    const stats = await invoke<{ cpu_usage: number; ram_usage: number }>("get_system_stats");
    cpuFill.style.width = `${stats.cpu_usage}%`;
    ramFill.style.width = `${stats.ram_usage}%`;
    cpuVal.textContent = `${Math.round(stats.cpu_usage)}%`;
    ramVal.textContent = `${Math.round(stats.ram_usage)}%`;
  } catch {}
  // Battery row — only shown when the device has a battery (laptop).
  try {
    const bat = await invoke<{ percent: number; charging: boolean } | null>("get_battery_status");
    const row = document.getElementById("battery-row");
    if (!bat || !row) { if (row) row.style.display = "none"; return; }
    row.style.display = "";
    const fill = document.getElementById("battery-fill")!;
    fill.style.width = `${bat.percent}%`;
    fill.style.background = bat.charging ? "#3fb950" : "";
    document.getElementById("battery-val")!.textContent = `${bat.percent}%${bat.charging ? " ⚡" : ""}`;
  } catch {}

}

// ===== Brightness widget (systemless dim overlay) =====
// A pure software dim: a click-through black overlay window whose alpha is
// the dim strength. Nothing on the system is modified — slide back to 0%
// (or exit Hush_UI) and the display is exactly as before.
const brightnessSlider = document.getElementById("brightness-slider") as HTMLInputElement;
const brightnessVal = document.getElementById("brightness-val")!;
const brightnessHint = document.getElementById("brightness-hint") as HTMLElement;

async function loadBrightness() {
  if (document.getElementById("brightness-widget")?.style.display === "none") return;
  try {
    // Read the persisted level from settings so the slider follows external
    // changes (settings table, startup restore).
    const s = await invoke<{ dimmer_level?: number }>("load_settings");
    const pct = Math.round((s.dimmer_level ?? 0) * 100);
    brightnessSlider.value = String(pct);
    brightnessVal.textContent = `${pct}%`;
  } catch {}
}

let brightnessTimer: number | null = null;
brightnessSlider.addEventListener("input", () => {
  const pct = parseInt(brightnessSlider.value, 10);
  brightnessVal.textContent = `${pct}%`;
  invoke("set_dimmer_level", { level: pct / 100 });
  // Persist (debounced) so the level survives restarts.
  if (brightnessTimer) window.clearTimeout(brightnessTimer);
  brightnessTimer = window.setTimeout(async () => {
    try {
      const s = await invoke<Record<string, unknown>>("load_settings");
      await invoke("save_settings", { settings: { ...s, dimmer_level: pct / 100 } });
    } catch {}
  }, 250);
});

// Elevation warning: without admin (UAC declined) the overlay may not dim
// system/elevated apps.
(async () => {
  try {
    const st = await invoke<{ elevated?: boolean; uac_declined?: boolean }>("get_elevation_state");
    if (st.uac_declined || st.elevated === false) {
      brightnessHint.hidden = false;
    }
  } catch {}
})();

// ===== Audio widget =====
async function loadAudio() {
  if (document.getElementById("audio-widget")?.style.display === "none") return;
  try {
    const vol = await invoke<number>("get_volume");
    const pct = Math.round(vol * 100);
    audioSlider.value = String(pct);
    audioVal.textContent = `${pct}%`;
  } catch {}
}

let audioDebounce: number | null = null;
audioSlider.addEventListener("input", () => {
  const vol = parseInt(audioSlider.value, 10) / 100;
  audioVal.textContent = `${audioSlider.value}%`;
  if (audioDebounce) window.clearTimeout(audioDebounce);
  audioDebounce = window.setTimeout(() => {
    invoke("set_volume", { volume: vol });
    void emit("volume://changed", vol);
  }, 50);
});

listen<number>("volume://changed", (e) => {
  const pct = Math.round(e.payload * 100);
  audioSlider.value = String(pct);
  audioVal.textContent = `${pct}%`;
});

// ===== Clipboard widget =====
let clipboardItems: string[] = [];
let lastClipboardText = "";

function renderClipboard() {
  clipboardList.innerHTML = "";
  for (const item of clipboardItems.slice(0, 15)) {
    const el = document.createElement("div");
    el.className = "clipboard-item";
    el.textContent = item.length > 40 ? item.substring(0, 40) + "…" : item;
    el.title = item.startsWith("data:image/") ? t("wg.screenshot") : item;
    el.addEventListener("click", () => {
      if (item.startsWith("data:image/")) {
        invoke("set_clipboard_image", { dataUrl: item });
      } else {
        invoke("set_clipboard_text", { text: item });
      }
    });
    clipboardList.appendChild(el);
  }
  if (clipboardItems.length === 0) {
    const empty = document.createElement("div");
    empty.className = "clipboard-item";
    empty.style.color = "var(--sand-dim)";
    empty.textContent = t("wg.noHistory");
    clipboardList.appendChild(empty);
  }
}

async function pollClipboard() {
  if (document.visibilityState !== "visible") return;
  if (document.getElementById("clipboard-widget")?.style.display === "none") return;
  try {
    const text = await invoke<string | null>("get_clipboard_text");
    if (text && text !== lastClipboardText) {
      lastClipboardText = text;
      clipboardItems.unshift(text);
      clipboardItems = [...new Set(clipboardItems)];
      if (clipboardItems.length > 15) clipboardItems.pop();
      renderClipboard();
      invoke("save_clipboard", { items: clipboardItems });
    }
  } catch {}
}

clipboardClearBtn.addEventListener("click", () => {
  clipboardItems = [];
  lastClipboardText = "";
  renderClipboard();
  invoke("save_clipboard", { items: [] });
  invoke("set_clipboard_text", { text: "" });
});

// ===== Notes widget (remade — shared module) =====
// Numbered note buttons, + to create, − to arm delete mode. Buttons open
// independent note-<num> editor windows. See shared/notes-widget.ts.

// Theme + icon recolor come from the shared module — colors and per-theme
// icon-recolor values are always applied together.

listen<ThemePayload>("theme://changed", (e) => {
  applyTheme(e.payload);
});

listen<boolean>("icon-recolor://changed", (e) => {
  document.documentElement.classList.toggle("icon-recolor", e.payload);
});

// ===== Init =====
(async function init() {
  await initI18n();
  try {
    const theme = await invoke<ThemePayload | null>("get_active_theme");
    if (theme) applyTheme(theme);
  } catch {}
  try {
    if (await invoke<boolean>("load_icon_recolor").catch(() => false)) {
      document.documentElement.classList.add("icon-recolor");
    }
  } catch {}
  await applyVisibility();
  await renderGreeting();
  await initNotesWidget();
  initAudioApps("wg");
  await loadAudio();
  await loadBrightness();
  renderClipboard();
  try {
    const items = await invoke<string[]>("load_clipboard");
    clipboardItems = items;
    if (items.length > 0) lastClipboardText = items[0];
    renderClipboard();
  } catch {}
})();
