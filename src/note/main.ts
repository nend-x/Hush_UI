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
listen<number>("note://shown", (e) => {
  if (e.payload !== num) return;
  root.classList.remove("shown");
  void root.offsetWidth;
  root.classList.add("shown");
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
listen<NoteEntry[]>("notes://changed", (e) => {
  if (!Number.isFinite(num)) return;
  if (!e.payload.some((n) => n.num === num)) close();
});

// ===== Theme + icon recolor (same contract as the tables) =====
listen<ThemePayload>("theme://changed", (e) => {
  applyTheme(e.payload);
});

listen<boolean>("icon-recolor://changed", (e) => {
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
})();
