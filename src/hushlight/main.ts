/* =========================================================================
   HUSHLIGHT — the medium floating search window (0.2 split).
   Win tap / picker table 4 opens it. Search only: apps, system shortcuts,
   desktop items excluded (those live in the desktop table now).
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { applyTheme, type ThemePayload } from "../shared/theme";

interface SearchResult {
  id: string;
  name: string;
  path: string;
  icon_data_url: string | null;
  is_folder: boolean;
}

const root = document.getElementById("fl-root")!;
const input = document.getElementById("fl-search") as HTMLInputElement;
const resultsEl = document.getElementById("fl-results")!;
const hintEl = document.getElementById("fl-hint")!;

// ===== Theme + icon recolor =====
// The hushlight page previously only loaded the theme at startup — a live
// theme switch never reached it, so the window stayed in the startup theme
// ("hardcoded"). It now follows theme://changed / icon-recolor://changed
// like every other surface.
listen<ThemePayload>("theme://changed", (e) => {
  applyTheme(e.payload);
});

listen<boolean>("icon-recolor://changed", (e) => {
  document.documentElement.classList.toggle("icon-recolor", e.payload);
});

// ===== Show / hide with pop animations =====
let closing = false;

listen("hushlight://shown", () => {
  closing = false;
  cancelPendingSearch();
  input.value = "";
  resultsEl.innerHTML = "";
  resultsEl.classList.add("hidden");
  hintEl.classList.remove("hidden");
  root.classList.remove("shown");
  void root.offsetWidth;
  root.classList.add("shown");
  setTimeout(() => input.focus(), 60);
});

listen("hushlight://hidden", () => {
  playPopOut();
});

function playPopOut() {
  // Kill a pending search timer on every leave path (Esc, blur, launch,
  // Win-tap) so a query never fires against a window that is already going
  // away.
  cancelPendingSearch();
  if (closing) return;
  closing = true;
  root.classList.remove("shown");
  // Clear the backend open-state NOW. The blur/Esc close paths never went
  // through hide_launcher_animated (which is what normally clears
  // LAUNCHER_OPEN) — without this, the post-animation
  // launcher_close_finished report is swallowed by its state guard and the
  // window stays alive as an invisible phantom panel that keeps eating
  // clicks. close_launcher early-returns when a hide is already in flight
  // (Win-tap path), so nothing double-runs.
  invoke("close_launcher");
  // Report once the pop-out has actually played so the backend hides the
  // window right after the animation instead of on a wall-clock guess.
  setTimeout(() => invoke("launcher_close_finished"), 240);
}

// ===== Search =====
let results: SearchResult[] = [];
let selected = 0;
let searchTimer: number | null = null;
// 0.3.1: searches fire on an idle cooldown instead of per keystroke. Every
// input event resets a 1s timer, so exactly one query runs once typing has
// settled — typing "calculator" at speed hits the backend a single time,
// not once per letter. 0.3.0 already made each query cheap (persistent
// index fast path, icons only for the final top-20); the cooldown removes
// the per-letter work entirely.
const SEARCH_IDLE_MS = 1000;
// 0.3.0: monotonic search generation. Two overlapping invokes can resolve
// out of order; without this guard the LAST response would win even when it
// belongs to an older query, flickering the results list while typing.
let searchSeq = 0;

function cancelPendingSearch() {
  if (searchTimer) {
    window.clearTimeout(searchTimer);
    searchTimer = null;
  }
}

function clearResults() {
  searchSeq++;
  results = [];
  resultsEl.innerHTML = "";
  resultsEl.classList.add("hidden");
  hintEl.classList.remove("hidden");
}

input.addEventListener("input", () => {
  cancelPendingSearch();
  // An emptied field clears immediately — no reason to wait out the
  // cooldown just to watch the list disappear.
  if (!input.value.trim()) {
    clearResults();
    return;
  }
  searchTimer = window.setTimeout(runSearch, SEARCH_IDLE_MS);
});

async function runSearch() {
  cancelPendingSearch();
  const q = input.value.trim();
  if (!q) {
    clearResults();
    return;
  }
  const seq = ++searchSeq;
  try {
    const found = await invoke<SearchResult[]>("search_programs", { query: q.toLowerCase() });
    if (seq !== searchSeq) return; // a newer keystroke already superseded this response
    results = found;
  } catch {
    if (seq !== searchSeq) return;
    results = [];
  }
  // Field may have been cleared while the fetch was in flight.
  if (!input.value.trim()) return;
  selected = 0;
  renderResults();
}

function renderResults() {
  resultsEl.innerHTML = "";
  hintEl.classList.toggle("hidden", results.length > 0);
  resultsEl.classList.toggle("hidden", results.length === 0);

  if (results.length === 0) {
    const empty = document.createElement("div");
    empty.className = "fl-empty";
    empty.textContent = "Nothing found";
    resultsEl.appendChild(empty);
    return;
  }

  results.forEach((result, idx) => {
    const el = document.createElement("div");
    el.className = "spotlight-item";
    if (idx === selected) el.classList.add("selected");
    el.dataset.id = result.id;

    const iconWrap = document.createElement("div");
    iconWrap.className = "spotlight-item-icon";
    if (result.icon_data_url) {
      const img = document.createElement("img");
      img.src = result.icon_data_url;
      img.alt = result.name;
      iconWrap.appendChild(img);
    } else {
      const ph = document.createElement("span");
      ph.textContent = (result.name || "?")[0].toUpperCase();
      ph.style.cssText = "font-family:var(--font-display);font-size:14px;color:var(--text-muted);";
      iconWrap.appendChild(ph);
    }
    el.appendChild(iconWrap);

    const label = document.createElement("div");
    label.className = "spotlight-item-label";
    label.textContent = result.name;
    el.appendChild(label);

    const parts = result.path.split(/[\\/]/);
    const shortPath = parts.slice(-2, -1)[0] || parts.slice(-1)[0] || "";
    const pathEl = document.createElement("div");
    pathEl.className = "spotlight-item-path";
    pathEl.textContent = shortPath;
    pathEl.title = result.path;
    el.appendChild(pathEl);

    el.addEventListener("click", (e) => {
      if (e.shiftKey) {
        invoke("execute_run_admin", { command: result.path });
        playPopOut();
      } else {
        launch(result);
      }
    });
    el.addEventListener("mouseenter", () => {
      selected = idx;
      updateSelection();
    });

    resultsEl.appendChild(el);
  });
}

function updateSelection() {
  const items = resultsEl.querySelectorAll(".spotlight-item");
  items.forEach((el, idx) => {
    el.classList.toggle("selected", idx === selected);
    if (idx === selected) el.scrollIntoView({ block: "nearest", behavior: "smooth" });
  });
}

function launch(result: SearchResult) {
  const path = result.path;
  if (path === "flatui:reboot") invoke("reboot_system");
  else if (path === "flatui:shutdown") invoke("shutdown_system");
  else if (path === "flatui:addstartup") invoke("add_to_startup");
  else if (path === "flatui:removestartup") invoke("remove_from_startup");
  else if (path === "hushui:screensaver") {
    // Screensaver shortcut: play the pop-out, then launch the fullscreen
    // OLED screensaver window.
    invoke("show_screensaver");
  }
  else invoke("execute_run", { command: path });
  playPopOut();
}

// ===== Keyboard =====
input.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.preventDefault();
    playPopOut();
    return;
  }
  // Enter during the cooldown flushes the pending query right away —
  // without this, typing "calc" and hitting Enter immediately would find an
  // empty list and silently do nothing until the 1s timer elapsed. When
  // results are already on screen, Enter keeps launching the visible
  // selection (WYSIWYG) instead of waiting for a newer query.
  if (e.key === "Enter" && searchTimer && results.length === 0) {
    e.preventDefault();
    runSearch();
    return;
  }
  if (results.length === 0) return;
  if (e.key === "ArrowDown") {
    e.preventDefault();
    selected = Math.min(selected + 1, results.length - 1);
    updateSelection();
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    selected = Math.max(selected - 1, 0);
    updateSelection();
  } else if (e.key === "Enter") {
    e.preventDefault();
    const r = results[selected];
    if (!r) return;
    if (e.shiftKey) {
      invoke("execute_run_admin", { command: r.path });
      playPopOut();
    } else {
      launch(r);
    }
  }
});

// Click outside the hushlight window closes it (Spotlight behavior): when
// the window loses focus and focus doesn't come back within a beat, play
// the pop-out. The re-check handles the brief focus dance right after open.
getCurrentWindow().onFocusChanged(({ payload: focused }) => {
  if (!focused && !closing) {
    setTimeout(() => {
      if (!document.hasFocus() && !closing) playPopOut();
    }, 150);
  }
});

// ===== Init =====
(async function init() {
  try {
    const theme = await invoke<ThemePayload | null>("get_active_theme");
    if (theme) applyTheme(theme);
  } catch {}
})();
