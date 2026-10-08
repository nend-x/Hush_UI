/* =========================================================================
   LAUNCHER main — Spotlight + run dialog + window switcher
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen, emit } from "@tauri-apps/api/event";
import { initNotesWidget } from "../shared/notes-widget";
import { initAudioApps } from "../shared/audio-apps";
import { initI18n, t } from "../shared/i18n";

interface LauncherItem {
  id: string;
  name: string;
  path: string;
  icon_data_url: string | null;
  is_folder: boolean;
}

interface SearchResult {
  id: string;
  name: string;
  path: string;
  icon_data_url: string | null;
  is_folder: boolean;
}

interface WindowEntry {
  hwnd: number;
  title: string;
  icon_data_url: string | null;
}

let allItems: LauncherItem[] = [];
let filteredItems: LauncherItem[] = [];
let selectedIdx = 0;

// Spotlight results
let spotlightResults: SearchResult[] = [];
let spotlightSelectedIdx = 0;

const root = document.getElementById("launcher")!;
const expandOverlay = document.getElementById("expand-overlay")!;
const searchInput = document.getElementById("search") as HTMLInputElement;
const grid = document.getElementById("grid")!;
const spotlightResultsEl = document.getElementById("spotlight-results")!;
const actionsWrap = document.querySelector<HTMLElement>(".launcher-actions")!;
const searchWrap = document.querySelector<HTMLElement>(".launcher-search-wrap")!;
const appsBody = document.getElementById("apps-body")!;
const minimizeAllBtn = document.getElementById("minimize-all-btn")!;
const runBtn = document.getElementById("run-btn")!;
const exitBtn = document.getElementById("exit-btn")!;
const runDialog = document.getElementById("run-dialog")!;
const runInput = document.getElementById("run-input") as HTMLInputElement;
const runOkBtn = document.getElementById("run-ok")!;
const runCancelBtn = document.getElementById("run-cancel")!;
const runAdminBtn = document.getElementById("run-admin")!;
const clipboardList = document.getElementById("clipboard-list")!;
const cpuFill = document.getElementById("cpu-fill")!;
const ramFill = document.getElementById("ram-fill")!;
const cpuVal = document.getElementById("cpu-val")!;
const ramVal = document.getElementById("ram-val")!;
const audioMasterSlider = document.getElementById("audio-master-slider") as HTMLInputElement;
const audioMasterVal = document.getElementById("audio-master-val")!;
const settingsBtn = document.getElementById("settings-btn")!;
const settingsOverlay = document.getElementById("settings-overlay")!;
const settingsClose = document.getElementById("settings-close")!;
const clipboardClearBtn = document.getElementById("clipboard-clear")!;

// Widget toggle checkboxes
const toggleClipboardWidget = document.getElementById("toggle-clipboard-widget") as HTMLInputElement;
const toggleNotesWidget = document.getElementById("toggle-notes-widget") as HTMLInputElement;
const toggleSysmonWidget = document.getElementById("toggle-sysmon-widget") as HTMLInputElement;
const toggleAudioWidget = document.getElementById("toggle-audio-widget") as HTMLInputElement;
const toggleAppsWidget = document.getElementById("toggle-apps-widget") as HTMLInputElement;
const toggleIconRecolor = document.getElementById("toggle-icon-recolor") as HTMLInputElement;
const themeSelect = document.getElementById("theme-select") as HTMLSelectElement;

// ===== Clipboard widget =====
let clipboardItems: string[] = [];
let lastClipboardText = "";

async function loadClipboardWidget() {
  try {
    clipboardItems = await invoke<string[]>("load_clipboard");
    if (clipboardItems.length > 0) {
      lastClipboardText = clipboardItems[0];
    }
    renderClipboard();
  } catch {}
}

// Poll clipboard every 500ms via Rust backend (catches global Ctrl+C)
async function pollClipboard() {
  // Skip while the launcher window is hidden — same reason as updateSysmon:
  // a bare 500 ms setInterval keeps firing while the user is doing other
  // things, and on Modern-Standby wake the queued timers burst-fire and
  // flood the IPC layer. When the launcher is shown again the next tick
  // (≤ 500 ms) captures the current clipboard contents, so the latest item
  // is never lost — only rapid multi-copies while the launcher is hidden
  // are not all captured (an acceptable tradeoff vs. the idle hang).
  if (document.visibilityState !== "visible") return;
  try {
    const text = await invoke<string | null>("get_clipboard_text");
    if (text && text !== lastClipboardText) {
      lastClipboardText = text;
      // Only add text items (not images)
      if (text.startsWith("data:image/")) {
        // It's an image data URL — add to clipboard
        clipboardItems.unshift(text);
      } else {
        clipboardItems.unshift(text);
      }
      // Deduplicate
      clipboardItems = [...new Set(clipboardItems)];
      if (clipboardItems.length > 15) clipboardItems.pop();
      renderClipboard();
      invoke("save_clipboard", { items: clipboardItems });
    }
  } catch {}
}

// Start polling
// 0.3.0 idle fix: polling is STARTED/STOPPED by launcher://force-shown and
// launcher://force-hidden (emitted by the backend when the launcher window
// is actually shown/hidden). No blind timers while the window is hidden.
let clipboardPollTimer: number | null = null;
let sysmonPollTimer: number | null = null;

function startPolling() {
  stopPolling();
  clipboardPollTimer = window.setInterval(pollClipboard, 500);
  sysmonPollTimer = window.setInterval(updateSysmon, 2000);
}

function stopPolling() {
  if (clipboardPollTimer !== null) {
    window.clearInterval(clipboardPollTimer);
    clipboardPollTimer = null;
  }
  if (sysmonPollTimer !== null) {
    window.clearInterval(sysmonPollTimer);
    sysmonPollTimer = null;
  }
}

// ===== Clipboard clear button =====
clipboardClearBtn.addEventListener("click", () => {
  clipboardItems = [];
  lastClipboardText = "";
  renderClipboard();
  invoke("save_clipboard", { items: [] });
  // Clear system clipboard too
  invoke("set_clipboard_text", { text: "" });
});

function renderClipboard() {
  clipboardList.innerHTML = "";
  for (const item of clipboardItems.slice(0, 15)) {
    const el = document.createElement("div");
    el.className = "clipboard-item";
    el.textContent = item.length > 40 ? item.substring(0, 40) + "…" : item;
    el.title = item.startsWith("data:image/") ? t("ln.screenshot") : item;
    el.addEventListener("click", () => {
      // Image items go back to the clipboard as a REAL image, not text
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
    empty.textContent = t("ln.noHistory");
    clipboardList.appendChild(empty);
  }
}

// ===== Notes widget (remade — shared module) =====
// Numbered note buttons, + to create, − to arm delete mode. Buttons open
// independent note-<num> editor windows. See shared/notes-widget.ts.

// ===== Sysmon widget =====
async function updateSysmon() {
  // Skip while the launcher window is hidden — the launcher is `visible: false`
  // by default (only shown on Win-tap). Its webview is alive regardless, so a
  // bare setInterval would keep firing invoke() calls every 2 s while the user
  // is doing other things. Under Modern-Standby / display-sleep the WebView
  // throttles but does not pause JS — when the system resumes, all queued
  // timers fire in a burst, flooding the IPC layer and contributing to the
  // "click → Not Responding" hang after ~2 min idle. Visibility-gating the
  // poll prevents that burst at the source. The first visible tick after a
  // show captures fresh data within one interval (2 s).
  if (document.visibilityState !== "visible") return;
  try {
    const stats = await invoke<{ cpu_usage: number; ram_usage: number; ram_total_gb: number; ram_used_gb: number }>("get_system_stats");
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

// ===== Audio widget =====
async function loadAudioWidget() {
  try {
    const vol = await invoke<number>("get_volume");
    const pct = Math.round(vol * 100);
    audioMasterSlider.value = String(pct);
    audioMasterVal.textContent = `${pct}%`;
  } catch {}
}

let audioDebounce: number | null = null;
audioMasterSlider?.addEventListener("input", () => {
  const vol = parseInt(audioMasterSlider.value, 10) / 100;
  audioMasterVal.textContent = `${audioMasterSlider.value}%`;
  // Emit cross-window sync event
  if (audioDebounce) clearTimeout(audioDebounce);
  audioDebounce = window.setTimeout(() => {
    invoke("set_volume", { volume: vol });
    // Also emit event for taskbar to sync
    emit("volume://changed", vol);
  }, 50);
});

// Listen for volume changes from taskbar
listen<number>("volume://changed", (e) => {
  const pct = Math.round(e.payload * 100);
  audioMasterSlider.value = String(pct);
  audioMasterVal.textContent = `${pct}%`;
});

// ===== Widget dragging =====
function makeDraggable(el: HTMLElement) {
  const header = el.querySelector(".widget-header") as HTMLElement | null;
  if (!header) return;

  let isDragging = false;
  let didDrag = false; // true if the mouse actually moved during the drag
  let startX = 0;
  let startY = 0;
  let origLeft = 0;
  let origTop = 0;
  const DRAG_THRESHOLD = 4; // px — below this, it's a click, not a drag

  header.addEventListener("mousedown", (e) => {
    // Don't drag if clicking inside textarea/input
    if ((e.target as HTMLElement).tagName === "INPUT" || (e.target as HTMLElement).tagName === "TEXTAREA") return;
    isDragging = true;
    didDrag = false;
    startX = e.clientX;
    startY = e.clientY;
    const rect = el.getBoundingClientRect();
    origLeft = rect.left;
    origTop = rect.top;
    // Switch from right/bottom positioning to left/top
    el.style.left = `${origLeft}px`;
    el.style.top = `${origTop}px`;
    el.style.right = "auto";
    el.style.bottom = "auto";
    e.preventDefault();
  });

  document.addEventListener("mousemove", (e) => {
    if (!isDragging) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (!didDrag && (Math.abs(dx) > DRAG_THRESHOLD || Math.abs(dy) > DRAG_THRESHOLD)) {
      didDrag = true;
    }
    if (didDrag) {
      el.style.left = `${origLeft + dx}px`;
      el.style.top = `${origTop + dy}px`;
    }
  });

  document.addEventListener("mouseup", () => {
    if (isDragging) {
      isDragging = false;
      // If we actually dragged, save positions and suppress the next click
      // so the widget's click handler doesn't fire (e.g. opening the
      // window switcher after moving the apps-widget).
      if (didDrag) {
        if (widgetSaveTimer) clearTimeout(widgetSaveTimer);
        widgetSaveTimer = window.setTimeout(saveWidgetPositions, 300);
        // Suppress the next click event on this element (the click that
        // follows mouseup after a drag). We capture it on the capture phase
        // and stop it.
        const suppressClick = (ev: Event) => {
          ev.stopPropagation();
          ev.preventDefault();
          el.removeEventListener("click", suppressClick, true);
        };
        el.addEventListener("click", suppressClick, true);
        // Also suppress on the header itself (the click might target the
        // header element, not the widget container).
        const suppressHeaderClick = (ev: Event) => {
          ev.stopPropagation();
          ev.preventDefault();
          header.removeEventListener("click", suppressHeaderClick, true);
        };
        header.addEventListener("click", suppressHeaderClick, true);
      }
    }
  });
}

// Make all widgets draggable
function initWidgetDragging() {
  document.querySelectorAll(".widget").forEach((el) => {
    makeDraggable(el as HTMLElement);
  });
}

// Save widget positions
let widgetSaveTimer: number | null = null;
function saveWidgetPositions() {
  const positions: Record<string, [number, number]> = {};
  document.querySelectorAll(".widget").forEach((el) => {
    const widget = el as HTMLElement;
    const id = widget.id;
    if (!id) return;
    const left = parseFloat(widget.style.left || "0");
    const top = parseFloat(widget.style.top || "0");
    if (left > 0 || top > 0) {
      positions[id] = [left, top];
    }
  });
  invoke("save_widget_positions", { positions });
}

// Load widget positions
async function loadWidgetPositions() {
  try {
    const positions = await invoke<Record<string, [number, number]>>("load_widget_positions");
    for (const [id, [left, top]] of Object.entries(positions)) {
      const el = document.getElementById(id);
      if (el) {
        el.style.left = `${left}px`;
        el.style.top = `${top}px`;
        el.style.right = "auto";
        el.style.bottom = "auto";
        el.style.transform = "none";
      }
    }
  } catch {}
}

// ===== Widget visibility settings =====
const WIDGET_IDS = [
  "clipboard-widget",
  "notes-widget",
  "sysmon-widget",
  "audio-widget",
  "apps-widget",
] as const;

async function loadWidgetVisibilitySettings() {
  try {
    const visibility = await invoke<Record<string, boolean>>("load_widget_visibility");
    // Apply loaded visibility to each widget
    for (const id of WIDGET_IDS) {
      const visible = visibility[id] ?? true; // default to visible
      const el = document.getElementById(id);
      if (el) {
        el.style.display = visible ? "" : "none";
      }
      // Update the toggle checkbox
      const toggle = document.getElementById(`toggle-${id}`) as HTMLInputElement | null;
      if (toggle) {
        toggle.checked = visible;
      }
    }
  } catch {
    // If load fails, all widgets are visible by default
  }
}

async function saveWidgetVisibility() {
  const visibility: Record<string, boolean> = {};
  for (const id of WIDGET_IDS) {
    const toggle = document.getElementById(`toggle-${id}`) as HTMLInputElement | null;
    if (toggle) {
      visibility[id] = toggle.checked;
    }
  }
  try {
    await invoke("save_widget_visibility", { visibility });
  } catch (err) {
    console.error("save_widget_visibility failed:", err);
  }
}

function applyWidgetVisibility(id: string, visible: boolean) {
  const el = document.getElementById(id);
  if (el) {
    el.style.display = visible ? "" : "none";
  }
}

// Settings button — open/close the settings overlay
settingsBtn.addEventListener("click", () => {
  settingsOverlay.classList.remove("hidden");
});

settingsClose.addEventListener("click", () => {
  settingsOverlay.classList.add("hidden");
});

settingsOverlay.addEventListener("click", (e) => {
  if (e.target === settingsOverlay) {
    settingsOverlay.classList.add("hidden");
  }
});

// Widget toggle handlers — apply immediately and save to config
toggleClipboardWidget.addEventListener("change", () => {
  applyWidgetVisibility("clipboard-widget", toggleClipboardWidget.checked);
  void saveWidgetVisibility();
});
toggleNotesWidget.addEventListener("change", () => {
  applyWidgetVisibility("notes-widget", toggleNotesWidget.checked);
  void saveWidgetVisibility();
});
toggleSysmonWidget.addEventListener("change", () => {
  applyWidgetVisibility("sysmon-widget", toggleSysmonWidget.checked);
  void saveWidgetVisibility();
});
toggleAudioWidget.addEventListener("change", () => {
  applyWidgetVisibility("audio-widget", toggleAudioWidget.checked);
  void saveWidgetVisibility();
});
toggleAppsWidget.addEventListener("change", () => {
  applyWidgetVisibility("apps-widget", toggleAppsWidget.checked);
  void saveWidgetVisibility();
});

// ===== Icon recolor toggle =====
// Applies the .icon-recolor class to the launcher root and emits an event
// so the taskbar can apply it too. Saves to config (icon_recolor.json).
function applyIconRecolor(enabled: boolean) {
  const launcherRoot = document.getElementById("launcher");
  if (launcherRoot) {
    if (enabled) {
      launcherRoot.classList.add("icon-recolor");
    } else {
      launcherRoot.classList.remove("icon-recolor");
    }
  }
  // Emit to the taskbar window so it can apply the same class
  emit("icon-recolor://changed", enabled);
}

async function loadIconRecolor() {
  try {
    const enabled = await invoke<boolean>("load_icon_recolor");
    toggleIconRecolor.checked = enabled;
    applyIconRecolor(enabled);
  } catch {
    // Default: off
  }
}

toggleIconRecolor.addEventListener("change", () => {
  applyIconRecolor(toggleIconRecolor.checked);
  invoke("save_icon_recolor", { enabled: toggleIconRecolor.checked });
});

// ===== Theme switcher =====
// Loads the active theme from the backend, applies its colors as CSS
// variables on :root, and saves when the user picks a different theme.
// The CSS variable names match the serde-renamed ThemeColors field names
// (e.g. bg-espresso → --bg-espresso, sand-cream → --sand-cream).

interface ThemeColors {
  "bg-espresso": string;
  "bg-espresso-rgb": string;
  "bg-espresso-deep": string;
  "bg-espresso-deep-rgb": string;
  "bg-espresso-raised": string;
  "bg-espresso-raised-rgb": string;
  "bg-espresso-frosted": string;
  "bg-espresso-glass": string;
  sand: string;
  "sand-rgb": string;
  "sand-bright": string;
  "sand-bright-rgb": string;
  "sand-dim": string;
  "sand-dim-rgb": string;
  "sand-cream": string;
  "sand-cream-rgb": string;
  "accent-terracotta": string;
  "accent-terracotta-rgb": string;
  "accent-caramel": string;
  "accent-caramel-rgb": string;
  "accent-soft": string;
  "border-subtle": string;
  "border-strong": string;
  "status-running": string;
  "status-pinned": string;
}

interface Theme {
  name: string;
  active: boolean;
  colors: ThemeColors;
}

// Per-theme icon recolor hue/sat values. These override the defaults in
// theme.css when a theme is applied, so icon recoloring matches the theme's
// accent color. Computed from each theme's accent-terracotta hue/saturation.
const THEME_ICON_RECOLOR: Record<string, { hue: string; sat: string; brightness: string }> = {
  // Neutral gray theme — recolor filter yields desaturated gray icons
  "material3-dark": { hue: "0deg", sat: "0.0", brightness: "0.95" },
};

function applyTheme(theme: Theme) {
  const root = document.documentElement;
  // Apply EVERY color from the theme as a CSS variable on :root.
  // The keys match the serde-renamed ThemeColors field names, so
  // "bg-espresso" → --bg-espresso, "bg-espresso-rgb" → --bg-espresso-rgb, etc.
  // This covers all solid colors AND all RGB-channel forms (used for
  // rgba(var(--xxx-rgb), alpha) compositions in the CSS).
  const colors = theme.colors;
  for (const [key, value] of Object.entries(colors)) {
    root.style.setProperty("--" + key, value);
  }

  // Apply per-theme icon recolor values
  const recolor = THEME_ICON_RECOLOR[theme.name];
  if (recolor) {
    root.style.setProperty("--icon-recolor-hue", recolor.hue);
    root.style.setProperty("--icon-recolor-sat", recolor.sat);
    root.style.setProperty("--icon-recolor-brightness", recolor.brightness);
  }

  // Emit to the taskbar so it can apply the same theme
  emit("theme://changed", theme);
}

async function loadActiveTheme() {
  try {
    const theme = await invoke<Theme | null>("get_active_theme");
    if (theme) {
      themeSelect.value = theme.name;
      applyTheme(theme);
    }
  } catch (err) {
    console.error("get_active_theme failed:", err);
  }
}

themeSelect.addEventListener("change", () => {
  const themeName = themeSelect.value;
  invoke("set_active_theme", { name: themeName });
  // Re-load the full theme to get its colors, then apply
  invoke<Theme | null>("get_active_theme").then((theme) => {
    if (theme) applyTheme(theme);
  });
});

// ===== Launcher open/close animation state machine =====
//
// Why this exists (bugfix): the old implementation sequenced the open
// animation with wall-clock setTimeouts that were never cancelled. Toggling
// faster than an animation ran let stale timers from a previous open fire
// mid-animation — the background snapped to its final state and elements
// appeared before the cube finished. On top of that, the cube animation was
// started while the window was still hidden, so under heavy GPU load
// (games) the animation clock ran ahead of the first presented frame and
// the cube appeared speeded-up or fully skipped.
//
// The rules now:
//   1. Every show/hide request bumps a session token. Async continuations
//      (rAF waits, animationend waits, fallback timers) capture the token
//      and bail out silently if it went stale — spamming the Win key can
//      never leave orphaned callbacks mutating the DOM.
//   2. The cube only starts once the window is actually visible AND the
//      compositor has produced frames (2× requestAnimationFrame) — the
//      animation clock and the presented output start together, no matter
//      how loaded the system is.
//   3. Sequencing is driven by real `animationend` events, not guessed
//      milliseconds. Fallback timers exist only as a safety net and are
//      session-guarded.
//   4. Show: cube expands → finishes → content elements fade in.
//      Hide:  content elements fade out → fully gone → THEN the cube
//      collapses → the DOM snaps clean and the backend is told to hide
//      the window at that exact moment. The close is the exact mirror of
//      the open — content must never float over a moving background.

let launcherSession = 0;
let sessionTimers: number[] = [];

// NOTE: the OS `prefers-reduced-motion` setting is deliberately IGNORED.
// Windows reports it whenever system animations are disabled (common on
// gaming machines with "best performance" visual effects), and honoring
// it made the launcher pop in with no animation at all. The shell pins
// its own motion policy — the animation always runs.

function trackTimer(id: number): void {
  sessionTimers.push(id);
}

function cancelSessionTimers(): void {
  for (const id of sessionTimers) clearTimeout(id);
  sessionTimers = [];
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    trackTimer(window.setTimeout(resolve, ms));
  });
}

function nextFrame(): Promise<number> {
  return new Promise((resolve) => requestAnimationFrame(resolve));
}

// Wait until the webview reports itself visible and has painted two frames.
// This guarantees the cube animation starts together with the first
// *presented* frame instead of against a hidden window (which made the
// animation appear "sped up" or missing under GPU load in games).
async function awaitOnScreen(session: number, maxWaitMs = 1500): Promise<boolean> {
  const t0 = performance.now();
  while (document.visibilityState !== "visible") {
    if (launcherSession !== session) return false;
    if (performance.now() - t0 > maxWaitMs) return false;
    await sleep(16);
  }
  // Two rAFs: the first may coalesce with the show-paint, the second
  // guarantees the compositor is actually producing frames.
  await nextFrame();
  if (launcherSession !== session) return false;
  await nextFrame();
  if (launcherSession !== session) return false;
  return true;
}

// Resolve when the named CSS animation really ends on `el` (its own clock,
// not a wall-clock guess). A safety timeout above the CSS duration covers
// swallowed events; both paths resolve identically. `animationcancel` (the
// class being ripped off by a newer session) also resolves so no caller
// can ever dangle.
function waitAnimationEnd(el: Element, animationName: string, safetyMs: number): Promise<void> {
  return new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      el.removeEventListener("animationend", onEnd);
      el.removeEventListener("animationcancel", onEnd);
      clearTimeout(safety);
      resolve();
    };
    const onEnd = (ev: Event) => {
      if ((ev as AnimationEvent).animationName === animationName) finish();
    };
    el.addEventListener("animationend", onEnd);
    el.addEventListener("animationcancel", onEnd);
    const safety = window.setTimeout(finish, safetyMs);
    trackTimer(safety);
  });
}

// Snap the launcher DOM to its clean pre-open state. Idempotent; safe from
// any session (in-flight callbacks are already session-guarded).
function resetLauncherDom(): void {
  root.classList.add("hidden");
  root.classList.remove("opening", "closing");
  expandOverlay.classList.remove("expanding", "expanded", "collapsing", "fading");
  searchInput.value = "";
  runDialog.classList.add("hidden");
  document.querySelectorAll(".context-menu").forEach((el) => el.remove());
  document.querySelectorAll(".modal-backdrop").forEach((el) => el.remove());
  applyFilter();
}

// Show sequence: window on screen → cube expands → cube DONE → elements in.
async function showLauncherSequence(): Promise<void> {
  const session = ++launcherSession;
  cancelSessionTimers();
  resetLauncherDom();

  // Gate on real visibility + painted frames. If that ever times out
  // (exotic webview states), open without the cube rather than freeze —
  // graceful degradation instead of a desynced animation.
  const onScreen = await awaitOnScreen(session);
  if (launcherSession !== session) return;

  if (onScreen) {
    const ended = waitAnimationEnd(expandOverlay, "cube-expand", 1500);
    expandOverlay.classList.add("expanding");
    await ended;
    if (launcherSession !== session) return;
  }
  expandOverlay.classList.remove("expanding");
  expandOverlay.classList.add("expanded");

  // Elements fade in strictly AFTER the background has finished.
  root.classList.remove("hidden");
  root.classList.add("opening");
  searchInput.focus({ preventScroll: true });

  // Refresh the apps widget so the running-window preview is fresh.
  // Fire-and-forget — never block the launcher open on this.
  void populateAppsWidget();

  // Drop .opening once the element cascade has finished (longest delay
  // 400ms + 400ms duration + slack). Session-guarded, so a rapid
  // re-toggle can never carry it into the next session.
  trackTimer(
    window.setTimeout(() => {
      if (launcherSession === session) root.classList.remove("opening");
    }, 1000)
  );
}

// Hide sequence — the exact mirror of the open sequence, in two strict
// phases:
//   Phase 1: elements fade out over a fully-expanded, static background.
//            (The content must never float over a collapsing background —
//            it reads as broken layering.)
//   Phase 2: elements fully gone → cube collapses over a clean screen →
//            DOM snaps clean and the backend hides the window at exactly
//            that moment (Rust delays its own hide as a fallback).
async function hideLauncherSequence(): Promise<void> {
  const session = ++launcherSession;
  cancelSessionTimers();

  // Nothing visible to animate (e.g. instant-hide paths) → snap clean.
  if (root.classList.contains("hidden")) {
    resetLauncherDom();
    invoke("launcher_close_finished");
    return;
  }

  // Phase 1 — elements out. The background stays fully expanded beneath
  // them for the whole cascade.
  root.classList.remove("opening"); // don't let opening styles fight the closing ones
  root.classList.add("closing");
  await waitElementCascadeOut();
  if (launcherSession !== session) return; // re-shown mid-close
  root.classList.add("hidden"); // content layer fully gone before the bg moves

  // Phase 2 — the cube collapses. expanded is removed and collapsing added
  // in one synchronous block, so no frame is ever painted in the base
  // (collapsed, invisible) state — the collapse animation starts FROM the
  // fullscreen appearance.
  expandOverlay.classList.remove("expanded");
  expandOverlay.classList.add("collapsing");
  await waitAnimationEnd(expandOverlay, "cube-collapse", 1500);
  if (launcherSession !== session) return; // re-shown mid-collapse
  resetLauncherDom();
  invoke("launcher_close_finished");
}

// Phase 1 helper: wait until the element fade-out cascade REALLY finished —
// `animationend` on every element actually running `launcher-element-out`.
// Elements that are display:none never start their animation, so they are
// filtered out and we never wait on an event that cannot fire.
function waitElementCascadeOut(): Promise<void> {
  const els = [actionsWrap, searchWrap, grid, spotlightResultsEl].filter(
    (el) => !el.classList.contains("hidden") && getComputedStyle(el).display !== "none"
  );
  if (els.length === 0) return Promise.resolve();
  return Promise.all(
    els.map((el) => waitAnimationEnd(el, "launcher-element-out", 1200))
  ).then(() => undefined);
}

function updateSelection() {
  const items = grid.querySelectorAll(".launcher-item");
  items.forEach((el, idx) => {
    if (idx === selectedIdx) {
      el.classList.add("selected");
      el.scrollIntoView({ block: "nearest", behavior: "smooth" });
    } else {
      el.classList.remove("selected");
    }
  });
}

function launch(item: LauncherItem | SearchResult) {
  const targetEl = findLaunchTarget(item);
  if (targetEl) {
    targetEl.classList.add("launching");
    // Stop spinning after 3 seconds
    setTimeout(() => {
      targetEl?.classList.remove("launching");
    }, 3000);
  }

  doLaunch(item);
  closeLauncher();
}

// Find the DOM element that matches this item (grid first, then spotlight).
// Items are matched by their unique id (full path) — NOT by display name.
// Matching by name made the launch animation (and with it the perceived
// click target) land on the first item that happened to share the label,
// e.g. spinning a FOLDER tile while actually launching an .exe that
// shares its label. Plain loops with early return — no closure assignment, so
// the compiler keeps the narrowing honest.
function findLaunchTarget(item: LauncherItem | SearchResult): HTMLElement | null {
  for (const el of Array.from(grid.querySelectorAll(".launcher-item"))) {
    if ((el as HTMLElement).dataset.id === item.id) {
      return el as HTMLElement;
    }
  }
  const spotlightItems = Array.from(spotlightResultsEl.querySelectorAll(".spotlight-item"));
  for (let idx = 0; idx < spotlightItems.length; idx++) {
    if (spotlightResults[idx] && spotlightResults[idx].id === item.id) {
      return spotlightItems[idx] as HTMLElement;
    }
  }
  // Fallback for items without an id-based match (e.g. system shortcuts):
  // compare label text, but only when no id collision is possible.
  for (const el of Array.from(grid.querySelectorAll(".launcher-item"))) {
    const labelEl = el.querySelector(".label");
    if (labelEl && labelEl.textContent === item.name && !(el as HTMLElement).dataset.id) {
      return el as HTMLElement;
    }
  }
  return null;
}

function doLaunch(item: LauncherItem | SearchResult) {
  const isDesktopItem = allItems.some((d) => d.id === item.id);
  if (isDesktopItem) {
    invoke("launch_desktop_item", { itemId: item.id });
  } else {
    // Check for special Hush_UI commands
    const path = item.path;
    if (path === "hush:reboot") {
      invoke("reboot_system");
    } else if (path === "hush:shutdown") {
      invoke("shutdown_system");
    } else if (path === "hush:addstartup") {
      invoke("add_to_startup");
    } else if (path === "hush:removestartup") {
      invoke("remove_from_startup");
    } else {
      invoke("execute_run", { command: path });
    }
  }
}

function closeLauncher() {
  invoke("close_launcher");
}

// External close (from Win key toggle, or close_launcher call) — animated.
listen("launcher://force-hidden", () => {
  // 0.3.0 idle fix: stop the clipboard/sysmon polling loops while the
  // window is hidden. The webview stays alive, and a bare setInterval kept
  // waking the IPC layer forever (with queued-timer burst-fires after
  // Modern-Standby resume).
  stopPolling();
  void hideLauncherSequence();
});

// External show (from Win key toggle) — cube first, elements after.
listen("launcher://force-shown", () => {
  startPolling();
  void showLauncherSequence();
});

// ===== Spotlight: when search non-empty, hide grid + show search results =====
let spotlightSearchTimer: number | null = null;

async function applyFilter() {
  const q = searchInput.value.trim().toLowerCase();

  if (q === "") {
    // Empty: the desktop grid is gone from hushlight (0.2 split — desktop
    // icons live in the desktop table now), so idle state shows nothing
    // but the centered search bar.
    if (spotlightSearchTimer) {
      window.clearTimeout(spotlightSearchTimer);
      spotlightSearchTimer = null;
    }
    filteredItems = [];
    selectedIdx = 0;
    grid.classList.add("hidden");
    spotlightResultsEl.classList.add("hidden");
    spotlightResultsEl.innerHTML = "";
    spotlightResults = [];
    return;
  }

  // Non-empty: desktop is EXCLUDED from search — only installed programs
  // and system shortcuts are searched (via search_programs). The desktop
  // grid is hidden while typing.
  filteredItems = [];
  selectedIdx = 0;
  grid.classList.add("hidden");

  // Debounced search_programs call
  if (spotlightSearchTimer) window.clearTimeout(spotlightSearchTimer);
  spotlightSearchTimer = window.setTimeout(async () => {
    // Check if search field is still non-empty (user may have cleared it during debounce)
    if (!searchInput.value.trim()) {
      return;
    }
    try {
      const remoteResults = await invoke<SearchResult[]>("search_programs", { query: q });
      // Double-check: user may have cleared the field while we were fetching
      if (!searchInput.value.trim()) {
        return;
      }
      spotlightResults = remoteResults;
      spotlightSelectedIdx = 0;
      renderSpotlightResults();
    } catch (err) {
      console.error("search_programs failed:", err);
    }
  }, 150);
}

function renderSpotlightResults() {
  spotlightResultsEl.innerHTML = "";

  if (spotlightResults.length === 0) {
    // If no results at all, show "not found" message
    if (filteredItems.length === 0) {
      grid.classList.add("hidden");
      const empty = document.createElement("div");
      empty.style.cssText = "color:var(--sand-dim);font-size:12px;padding:24px;text-align:center;";
      empty.textContent = t("ln.nothingFound");
      spotlightResultsEl.appendChild(empty);
      spotlightResultsEl.classList.remove("hidden");
    }
    return;
  }

  // Show spotlight results panel
  spotlightResultsEl.classList.remove("hidden");
  if (filteredItems.length === 0) {
    grid.classList.add("hidden");
  }

  spotlightResults.forEach((result, idx) => {
    const el = document.createElement("div");
    el.className = "spotlight-item";
    if (idx === spotlightSelectedIdx) el.classList.add("selected");
    el.dataset.id = result.id;

    const iconWrap = document.createElement("div");
    iconWrap.className = "spotlight-item-icon";
    if (result.icon_data_url) {
      const img = document.createElement("img");
      img.src = result.icon_data_url;
      img.alt = result.name;
      iconWrap.appendChild(img);
    } else {
      const placeholder = document.createElement("span");
      placeholder.textContent = (result.name || "?")[0].toUpperCase();
      placeholder.style.cssText = "font-family:var(--font-display);font-size:14px;color:var(--sand-dim);";
      iconWrap.appendChild(placeholder);
    }
    el.appendChild(iconWrap);

    const label = document.createElement("div");
    label.className = "spotlight-item-label";
    label.textContent = result.name;
    el.appendChild(label);

    // Truncated path
    const pathEl = document.createElement("div");
    pathEl.className = "spotlight-item-path";
    // Just the parent directory name as a hint
    const pathParts = result.path.split(/[\\/]/);
    const shortPath = pathParts.slice(-2, -1)[0] || pathParts.slice(-1)[0] || "";
    pathEl.textContent = shortPath;
    pathEl.title = result.path;
    el.appendChild(pathEl);

    el.addEventListener("click", (e) => {
      if (e.shiftKey) {
        invoke("execute_run_admin", { command: result.path });
        closeLauncher();
      } else {
        launch(result);
      }
    });
    el.addEventListener("mouseenter", () => {
      spotlightSelectedIdx = idx;
      updateSpotlightSelection();
    });

    spotlightResultsEl.appendChild(el);
  });
}

function updateSpotlightSelection() {
  const items = spotlightResultsEl.querySelectorAll(".spotlight-item");
  items.forEach((el, idx) => {
    if (idx === spotlightSelectedIdx) {
      el.classList.add("selected");
      el.scrollIntoView({ block: "nearest", behavior: "smooth" });
    } else {
      el.classList.remove("selected");
    }
  });
}

// ===== Keyboard =====
searchInput.addEventListener("input", applyFilter);

document.addEventListener("keydown", (e) => {
  // Win key detection (fallback for when the Rust-level hook doesn't fire
  // while the launcher has focus). On Windows, the Win key is reported as
  // e.key === "Meta" and e.code === "MetaLeft" or "MetaRight". When the
  // launcher is open and the user taps Win, close the launcher — this
  // mirrors the Rust hook's toggle behavior for the close-tap case.
  if (e.key === "Meta" || e.code === "MetaLeft" || e.code === "MetaRight") {
    e.preventDefault();
    closeLauncher();
    return;
  }

  // If focus is in notes textarea or run input, don't process launcher shortcuts
  const target = e.target as HTMLElement;
  if (target.tagName === "TEXTAREA" || (target.tagName === "INPUT" && target.id !== "search")) {
    return;
  }

  // Run dialog has its own keyboard handling
  if (!runDialog.classList.contains("hidden")) {
    if (e.key === "Enter") {
      e.preventDefault();
      executeRunFromInput();
    } else if (e.key === "Escape") {
      e.preventDefault();
      hideRunDialog();
    }
    return;
  }

  if (e.key === "Escape") {
    e.preventDefault();
    closeLauncher();
    return;
  }

  // Spotlight navigation
  const q = searchInput.value.trim();
  if (q && spotlightResults.length > 0) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      spotlightSelectedIdx = Math.min(spotlightSelectedIdx + 1, spotlightResults.length - 1);
      updateSpotlightSelection();
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      spotlightSelectedIdx = Math.max(spotlightSelectedIdx - 1, 0);
      updateSpotlightSelection();
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      if (spotlightResults[spotlightSelectedIdx]) {
        launch(spotlightResults[spotlightSelectedIdx]);
      }
      return;
    }
    return; // don't process launcher grid keys when spotlight active
  }

  // Regular grid navigation
  if (filteredItems.length === 0) return;
  if (e.key === "Enter") {
    e.preventDefault();
    if (filteredItems[selectedIdx]) launch(filteredItems[selectedIdx]);
    return;
  }
  const cols = parseInt(
    getComputedStyle(document.documentElement).getPropertyValue("--launcher-grid-cols").trim(),
    10
  ) || 8;
  let newIdx = selectedIdx;
  if (e.key === "ArrowRight") newIdx = Math.min(selectedIdx + 1, filteredItems.length - 1);
  else if (e.key === "ArrowLeft") newIdx = Math.max(selectedIdx - 1, 0);
  else if (e.key === "ArrowDown") newIdx = Math.min(selectedIdx + cols, filteredItems.length - 1);
  else if (e.key === "ArrowUp") newIdx = Math.max(selectedIdx - cols, 0);
  if (newIdx !== selectedIdx) {
    e.preventDefault();
    selectedIdx = newIdx;
    updateSelection();
  }
});

// Right-click → context menu (desktop grid only — hushlight is search-only
// since the 0.2 split, so it never shows the New Folder / Refresh menu).
root.addEventListener("contextmenu", (e) => {
  if (root.classList.contains("hushlight-mode")) {
    e.preventDefault();
    return;
  }
  if (e.target === root || e.target === grid) {
    e.preventDefault();
    showBackgroundContextMenu(e.clientX, e.clientY);
  }
});

root.addEventListener("click", (e) => {
  if (e.target === root || e.target === grid) {
    document.querySelectorAll(".context-menu").forEach((el) => el.remove());
  }
});

// ===== Action buttons =====
// Apps widget: a compact preview of running windows that lives where the
// old `winselect-btn` action button used to be. Click any tile to focus
// that window; click the widget chrome (header / empty body) to open the
// full window-switcher overlay (same behavior as the old button).
async function populateAppsWidget() {
  let windows: WindowEntry[] = [];
  try {
    windows = await invoke<WindowEntry[]>("get_all_windows");
  } catch (err) {
    console.error("get_all_windows failed (apps-widget):", err);
  }

  appsBody.innerHTML = "";

  if (windows.length === 0) {
    const empty = document.createElement("div");
    empty.className = "apps-empty";
    empty.textContent = t("ln.noWindows");
    appsBody.appendChild(empty);
    return;
  }

  // Show ALL running windows — no limit, no +N badge. The widget
  // auto-sizes to fit (flex-wrap handles overflow to multiple rows).
  for (const w of windows) {
    const tile = document.createElement("div");
    tile.className = "apps-tile";
    tile.title = w.title;

    if (w.icon_data_url) {
      const img = document.createElement("img");
      img.src = w.icon_data_url;
      img.alt = w.title;
      tile.appendChild(img);
    } else {
      const fb = document.createElement("span");
      fb.className = "apps-tile-fallback";
      fb.textContent = (w.title || "?").charAt(0).toUpperCase();
      tile.appendChild(fb);
    }

    tile.addEventListener("click", (e) => {
      e.stopPropagation();
      invoke("activate_window", { hwnd: w.hwnd });
      closeLauncher();
    });

    appsBody.appendChild(tile);
  }
}

// Apps widget — no switcher overlay. Clicks on tiles focus the window.
// Clicks on empty space do nothing.

minimizeAllBtn.addEventListener("click", () => {
  // The Rust handler will hide the launcher itself.
  invoke("minimize_all_windows");
  minimizeAllBtn.classList.add("active");
  setTimeout(() => minimizeAllBtn.classList.remove("active"), 300);
});

// Exit button — reverts everything (kills start-menu killer, shows taskbar,
// restarts explorer) and exits the app. The Rust command handles all the
// cleanup; we just invoke it.
exitBtn.addEventListener("click", () => {
  exitBtn.classList.add("active");
  invoke("exit_hush");
});

runBtn.addEventListener("click", () => showRunDialog());


// ===== Run dialog =====
function showRunDialog() {
  runDialog.classList.remove("hidden");
  runInput.value = "";
  setTimeout(() => runInput.focus(), 50);
}

function hideRunDialog() {
  runDialog.classList.add("hidden");
  runInput.value = "";
  searchInput.focus();
}

function executeRunFromInput() {
  const cmd = runInput.value.trim();
  if (cmd) {
    invoke("execute_run", { command: cmd });
    hideRunDialog();
    closeLauncher();
  }
}

runOkBtn.addEventListener("click", executeRunFromInput);
runCancelBtn.addEventListener("click", hideRunDialog);
runAdminBtn.addEventListener("click", () => {
  const cmd = runInput.value.trim();
  if (cmd) {
    invoke("execute_run_admin", { command: cmd });
    hideRunDialog();
    closeLauncher();
  }
});


// ===== Context menus =====
function showBackgroundContextMenu(x: number, y: number) {
  document.querySelectorAll(".context-menu").forEach((el) => el.remove());

  const menu = document.createElement("div");
  menu.className = "context-menu";
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;

  const newItem = document.createElement("div");
  newItem.className = "context-menu-item";
  newItem.innerHTML = `<span>${t("ln.new")}</span><span class="context-menu-submenu-arrow">▸</span>`;

  const submenu = document.createElement("div");
  submenu.className = "context-menu-submenu";

  const folderItem = document.createElement("div");
  folderItem.className = "context-menu-item";
  folderItem.textContent = t("ln.folder");
  folderItem.addEventListener("click", () => {
    menu.remove();
    showNewFolderDialog();
  });

  const fileItem = document.createElement("div");
  fileItem.className = "context-menu-item";
  fileItem.textContent = t("ln.file");
  fileItem.addEventListener("click", () => {
    menu.remove();
    showNewFileDialog();
  });

  submenu.appendChild(folderItem);
  submenu.appendChild(fileItem);
  newItem.appendChild(submenu);
  menu.appendChild(newItem);

  const sep = document.createElement("div");
  sep.className = "context-menu-separator";
  menu.appendChild(sep);

  const refresh = document.createElement("div");
  refresh.className = "context-menu-item";
  refresh.textContent = t("ln.refresh");
  refresh.addEventListener("click", () => {
    menu.remove();
    invoke("refresh_desktop");
  });
  menu.appendChild(refresh);

  document.body.appendChild(menu);

  const rect = menu.getBoundingClientRect();
  if (rect.right > window.innerWidth - 8) menu.style.left = `${x - rect.width}px`;
  if (rect.bottom > window.innerHeight - 8) menu.style.top = `${y - rect.height}px`;
}

// ===== Dialogs =====
function showNewFolderDialog() {
  showDialog(t("ln.newFolder"), "", t("ln.folderName"), (name) => {
    invoke("create_desktop_item", { name, isFolder: true });
  });
}

function showNewFileDialog() {
  showDialog(t("ln.newFile"), "", t("ln.extPh"), (name) => {
    invoke("create_desktop_item", { name, isFolder: false });
  });
}

function showDialog(
  title: string,
  initialValue: string,
  placeholder: string,
  onConfirm: (name: string) => void
) {
  document.querySelectorAll(".modal-backdrop").forEach((el) => el.remove());

  const backdrop = document.createElement("div");
  backdrop.className = "modal-backdrop";

  const dialog = document.createElement("div");
  dialog.className = "modal-dialog";

  const titleEl = document.createElement("div");
  titleEl.className = "modal-title";
  titleEl.textContent = title;

  const input = document.createElement("input");
  input.className = "modal-input";
  input.type = "text";
  input.value = initialValue;
  input.placeholder = placeholder;
  input.autofocus = true;

  const actions = document.createElement("div");
  actions.className = "modal-actions";

  const cancel = document.createElement("button");
  cancel.className = "modal-btn";
  cancel.textContent = t("ln.cancel");

  const confirm = document.createElement("button");
  confirm.className = "modal-btn primary";
  confirm.textContent = t("ln.ok");

  actions.appendChild(cancel);
  actions.appendChild(confirm);
  dialog.appendChild(titleEl);
  dialog.appendChild(input);
  dialog.appendChild(actions);
  backdrop.appendChild(dialog);
  document.body.appendChild(backdrop);

  setTimeout(() => input.focus(), 10);

  const close = () => backdrop.remove();
  cancel.addEventListener("click", close);
  backdrop.addEventListener("click", (e) => {
    if (e.target === backdrop) close();
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      const v = input.value.trim();
      if (v) {
        onConfirm(v);
        close();
      }
    } else if (e.key === "Escape") {
      close();
    }
  });
  confirm.addEventListener("click", () => {
    const v = input.value.trim();
    if (v) {
      onConfirm(v);
      close();
    }
  });
}

// ===== Settings =====

// ===== Init =====
async function init() {
  await initI18n();
  try {
    allItems = await invoke<LauncherItem[]>("get_desktop_items");
    applyFilter();
  } catch (err) {
    console.error("get_desktop_items failed:", err);
    allItems = [];
    applyFilter();
  }

  await listen<LauncherItem[]>("launcher://items-updated", (e) => {
    allItems = e.payload;
    applyFilter();
  });

  // Load widgets
  loadClipboardWidget();
  void initNotesWidget();
  loadAudioWidget();
  initAudioApps("ln");
  // 0.3.0 idle fix: sysmon polling now starts/stops with the window
  // (launcher://force-shown / force-hidden) instead of running forever.
  updateSysmon();
  initWidgetDragging();
  await loadWidgetPositions();
  await loadWidgetVisibilitySettings();
  await loadIconRecolor();
  await loadActiveTheme();

  // (The running-build version badge used to live in the bottom-left
  // corner of the launcher. It has been removed from the surface — the
  // backend still reports the version via Tauri APIs if a tool needs it.)
  // NOTE: no focus timer here — the launcher page loads hidden; the show
  // sequence (showLauncherSequence) focuses the search input at the right
  // moment, after the background animation has finished.
}

init();
