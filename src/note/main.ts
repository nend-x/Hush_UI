/* =========================================================================
   Independent note editor window — opened by a numbered button on the
   notes widget. One window per note, label `note-<num>` (the number comes
   from the label, matching the backend's note-{num} labels).

   Same design language as the movable tables: mt-root chrome (dots, title,
   close), the launcher's lined-paper textarea. The window is created
   hidden by the backend, positioned with a cascade, then `note://shown`
   pops it in — the exact lifecycle the tables use.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import type { NoteEntry } from "../shared/notes-widget";
import { applyTheme, type ThemePayload } from "../shared/theme";

const win = getCurrentWindow();

// note-<num> — the backend guarantees this label shape.
const num = parseInt(win.label.split("-")[1] ?? "", 10);

const root = document.getElementById("note-root")!;
const title = document.getElementById("note-title")!;
const textarea = document.getElementById("note-text") as HTMLTextAreaElement;

if (Number.isFinite(num)) {
  title.textContent = `Note ${num}`;
  document.title = `Hush_UI — Note ${num}`;
}

// ===== Pop-in (backend emits note://shown with our num after show+focus)
function popIn() {
  root.classList.remove("shown");
  void root.offsetWidth;
  root.classList.add("shown");
}

// The listen() promises are kept — init must await them before telling the
// backend we're ready (see the 0.4.2 ghost-window fix note in init below).
const shownReady = listen<number>("note://shown", (e) => {
  if (e.payload !== num) return;
  popIn();
});

// ===== Save (debounced like the old widget textarea; blur flushes) =====
let saveTimer: number | null = null;

function saveNow() {
  if (saveTimer) {
    window.clearTimeout(saveTimer);
    saveTimer = null;
  }
  if (!Number.isFinite(num)) return;
  void invoke("note_save", { num, text: textarea.value });
}

textarea.addEventListener("input", () => {
  if (saveTimer) window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(saveNow, 500);
});
textarea.addEventListener("blur", saveNow);

// ===== Close (pop-out first, like every table) =====
let closing = false;

function close() {
  if (closing) return;
  closing = true;
  saveNow();
  root.classList.remove("shown");
  // Let the pop-out play before the window is destroyed.
  setTimeout(() => void win.close(), 200);
}

document.getElementById("note-close")!.addEventListener("click", close);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") close();
});

// ===== Deleted while open → the backend closes this window; belt and
// braces: if a change event arrives without our note, close ourselves.
const changedReady = listen<NoteEntry[]>("notes://changed", (e) => {
  if (!Number.isFinite(num)) return;
  if (!e.payload.some((n) => n.num === num)) close();
});

// ===== Theme + icon recolor (same contract as the tables) =====
const themeReady = listen<ThemePayload>("theme://changed", (e) => {
  applyTheme(e.payload);
});

const iconReady = listen<boolean>("icon-recolor://changed", (e) => {
  document.documentElement.classList.toggle("icon-recolor", e.payload);
});

// ===== Init =====
(async function init() {
  try {
    const theme = await invoke<ThemePayload | null>("get_active_theme");
    if (theme) applyTheme(theme);
  } catch {}
  try {
    if (await invoke<boolean>("load_icon_recolor").catch(() => false)) {
      document.documentElement.classList.add("icon-recolor");
    }
  } catch {}
  try {
    textarea.value = await invoke<string>("note_load", { num });
  } catch {}
  textarea.focus();

  // 0.4.2 ghost-window fix: the window is built hidden and the backend
  // reveals it ONLY on note_window_ready — which we invoke here, after
  // every listen() subscription has RESOLVED. 0.4.1 fired note://shown
  // while the page was still booting, the pop-in never ran and the
  // window stayed invisible (opacity:0) but clickable forever.
  await Promise.allSettled([shownReady, changedReady, themeReady, iconReady]);
  try {
    if (await win.isVisible()) popIn(); // fallback show won the race
  } catch {}
  void invoke("note_window_ready", { num }).catch(() => {});
})();
