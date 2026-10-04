/* =========================================================================
   Hush_UI — Notification framework (frontend)
   Listens for notify://show / notify://progress / notify://hide and plays
   the slide-in/out. Reports back with notification_close_finished when the
   slide-out is done so the backend can hide the window.

   0.3.0: payloads may carry `progress` (0-100) — the toast then renders a
   statusbar (used by the search index build). Progress UPDATES arrive via
   notify://progress and refresh the bar without replaying the slide-in.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { applyTheme, type ThemePayload } from "../shared/theme";

const toast = document.getElementById("toast")!;
const titleEl = document.getElementById("toast-title")!;
const bodyEl = document.getElementById("toast-body")!;
const progressEl = document.getElementById("toast-progress") as HTMLElement;
const progressFill = document.getElementById("toast-progress-fill") as HTMLElement;

interface ShowPayload {
  title: string;
  body: string;
  progress?: number;
}

listen<ShowPayload>("notify://show", (e) => {
  titleEl.textContent = e.payload.title;
  bodyEl.textContent = e.payload.body;
  // statusbar: only rendered when the payload carries a progress value
  if (typeof e.payload.progress === "number") {
    progressEl.hidden = false;
    progressFill.style.width = `${Math.max(0, Math.min(100, e.payload.progress))}%`;
  } else {
    progressEl.hidden = true;
    progressFill.style.width = "0%";
  }
  // restart the slide-in animation
  toast.classList.remove("in", "out");
  void toast.offsetWidth;
  toast.classList.add("in");
});

// In-place progress update — no animation replay, window untouched.
listen<{ progress?: number; body?: string }>("notify://progress", (e) => {
  if (typeof e.payload.body === "string") {
    bodyEl.textContent = e.payload.body;
  }
  if (typeof e.payload.progress === "number") {
    progressEl.hidden = false;
    progressFill.style.width = `${Math.max(0, Math.min(100, e.payload.progress))}%`;
  }
});

listen("notify://hide", () => {
  if (!toast.classList.contains("in") || toast.classList.contains("out")) return;
  toast.classList.add("out");
  // report back once the slide-out finished so the backend hides the window
  setTimeout(() => {
    toast.classList.remove("in", "out");
    invoke("notification_close_finished");
  }, 380);
});

// ===== Theme =====
// The toast used to never apply themes (--surface had no value, so it fell
// back to a hardcoded dark card under EVERY theme). Follow the active
// theme like every other surface: apply at startup, then live updates.
invoke<ThemePayload | null>("get_active_theme")
  .then((theme) => { if (theme) applyTheme(theme); })
  .catch(() => {});
listen<ThemePayload>("theme://changed", (e) => applyTheme(e.payload));
