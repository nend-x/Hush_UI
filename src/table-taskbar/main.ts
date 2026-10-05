/* =========================================================================
   TABLE 1 — taskbar table. The taskbar's own icons on a small vertical
   line: max 5 on screen, wheel-scrolls with a smooth animation, same
   click / right-click behavior as the real taskbar, macOS-style hover
   magnification (hovered icon only).

   The strip is PERSISTENT once opened: moving the cursor away never
   closes it (Esc or launching an app does). Backend app updates are
   reconciled INCREMENTALLY — existing icons keep their position and
   scroll offset, new icons append to the end.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { applyTheme, type ThemePayload } from "../shared/theme";
import { initI18n, t } from "../shared/i18n";

interface TaskbarApp {
  id: string;
  name: string;
  icon_data_url: string | null;
  running: boolean;
  pinned: boolean;
  is_foreground: boolean;
}

const root = document.getElementById("tt-root")!;
const scroll = document.getElementById("tt-scroll")!;
const names = document.getElementById("tt-names")!;

// Theme + icon-recolor values are applied together by the shared applyTheme
// — startup load AND live theme://changed events both go through it.

async function loadInitialTheme() {
  try {
    const theme = await invoke<ThemePayload | null>("get_active_theme");
    if (theme) applyTheme(theme);
    if (await invoke<boolean>("load_icon_recolor").catch(() => false)) {
      document.documentElement.classList.add("icon-recolor");
    }
  } catch {}
}

listen<ThemePayload>("theme://changed", (e) => {
  applyTheme(e.payload);
});
listen<boolean>("icon-recolor://changed", (e) => {
  document.documentElement.classList.toggle("icon-recolor", e.payload);
});

// ===== Magnification strength — hardcoded 1.45x (hovered icon only) =====
const magnify = 1.45;

// ===== App data + keyed icon reconciliation =====
// DOM order is append-only; the payload's order is IGNORED so a refresh can
// never yank the icons the user is currently looking at. New apps append to
// the end, gone apps are removed, live apps are updated in place.
let apps: TaskbarApp[] = [];
let appById = new Map<string, TaskbarApp>();

function appFor(el: HTMLElement): TaskbarApp | undefined {
  return appById.get(el.dataset.appId!);
}

function buildIcon(app: TaskbarApp): HTMLElement {
  const slot = document.createElement("div");
  slot.className = "tt-icon";
  slot.dataset.appId = app.id;

  const box = document.createElement("div");
  box.className = "tt-icon-box";
  slot.appendChild(box);

  const indicator = document.createElement("div");
  indicator.className = "indicator";
  slot.appendChild(indicator);

  slot.addEventListener("click", () => {
    const a = appFor(slot);
    if (!a) return;
    invoke("activate_app", { appId: a.id });
    box.style.animation = "icon-pop 280ms var(--ease-spring)";
    setTimeout(() => (box.style.animation = ""), 280);
    closeStrip();
  });

  slot.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    const a = appFor(slot);
    if (a) showContextMenu(slot, a);
  });

  slot.addEventListener("mouseenter", () => {
    hoveredIdx = iconEls().indexOf(slot);
    slot.classList.add("hover");
    slot.style.setProperty("--mag", magnify.toFixed(3));
  });

  slot.addEventListener("mouseleave", () => {
    if (iconEls().indexOf(slot) === hoveredIdx) {
      hoveredIdx = -1;
      slot.classList.remove("hover");
      slot.style.removeProperty("--mag");
    }
  });

  updateIconContent(slot, app);
  return slot;
}

function updateIconContent(slot: HTMLElement, app: TaskbarApp) {
  slot.classList.toggle("running", app.running);
  slot.classList.toggle("pinned", app.pinned);
  slot.classList.toggle("foreground", app.is_foreground);

  const box = slot.querySelector(".tt-icon-box") as HTMLElement;
  const img = box.querySelector("img");
  const letter = box.querySelector(".fallback");

  if (app.icon_data_url) {
    if (img) {
      if (img.src !== app.icon_data_url) img.src = app.icon_data_url;
    } else {
      box.innerHTML = "";
      const el = document.createElement("img");
      el.src = app.icon_data_url;
      el.alt = app.name;
      el.draggable = false;
      box.appendChild(el);
    }
  } else if (!letter) {
    box.innerHTML = "";
    const el = document.createElement("span");
    el.className = "fallback";
    el.textContent = (app.name || "?")[0].toUpperCase();
    box.appendChild(el);
  }
}

function reconcile() {
  appById = new Map(apps.map((a) => [a.id, a]));

  // Remove icons whose app vanished.
  for (const el of Array.from(scroll.children) as HTMLElement[]) {
    if (!appById.has(el.dataset.appId!)) {
      if (el.classList.contains("hover")) {
        hoveredIdx = -1;
      }
      el.remove();
    }
  }

  // Append new apps at the END (payload order never reorders existing icons).
  const present = new Set(
    Array.from(scroll.children).map((el) => (el as HTMLElement).dataset.appId)
  );
  for (const app of apps) {
    if (!present.has(app.id)) {
      scroll.appendChild(buildIcon(app));
    }
  }

  // Update live icons in place (running/pinned/foreground/icons/names).
  for (const el of Array.from(scroll.children) as HTMLElement[]) {
    const app = appById.get(el.dataset.appId!);
    if (app) updateIconContent(el, app);
  }

  // Clamp the scroll target into the new range without moving it otherwise.
  target = Math.max(0, Math.min(maxOffset(), target));
  rebuildNames();
}

function iconEls(): HTMLElement[] {
  return Array.from(scroll.querySelectorAll<HTMLElement>(".tt-icon"));
}

let hoveredIdx = -1;

// ===== Permanent name bars =====
// The old hover tooltip look, but one bar per icon, always visible. Bars
// live in a container OUTSIDE the clipping rail; the container mirrors the
// rail's scroll transform (applyTransform) so each bar stays glued to its
// icon. Bars are clickable = same activate behavior as the icon.
function rebuildNames() {
  names.innerHTML = "";
  for (const el of iconEls()) {
    const app = appFor(el);
    if (!app) continue;
    const bar = document.createElement("div");
    bar.className = "tt-name-bar";
    if (app.is_foreground) bar.classList.add("foreground");
    bar.textContent = app.name;
    bar.dataset.appId = app.id;
    // Vertical position: center of the icon slot (offsetTop + 31), the
    // pill then centers itself with translateY(-50%). The container's
    // shared scroll transform keeps it glued to the icon while scrolling.
    bar.style.top = `${el.offsetTop + 31}px`;
    bar.addEventListener("click", () => {
      invoke("activate_app", { appId: app.id });
      closeStrip();
    });
    names.appendChild(bar);
  }
}

// ===== Smooth wheel scrolling (max 5 icons visible) =====
const SLOT = 62;
const VISIBLE = 5;

let offset = 0;   // current (animated) offset
let target = 0;   // scroll target
let raf = 0;

function maxOffset(): number {
  const total = Math.max(0, iconEls().length * SLOT);
  const visible = VISIBLE * SLOT;
  return Math.max(0, total - visible);
}

function applyTransform() {
  scroll.style.transform = `translateY(${-offset}px)`;
  names.style.transform = `translateY(${-offset}px)`;
}

function animate() {
  // Critically-damped-ish lerp — smooth, never overshoots the edges.
  offset += (target - offset) * 0.22;
  if (Math.abs(target - offset) < 0.4) {
    offset = target;
    applyTransform();
    raf = 0;
    return;
  }
  applyTransform();
  raf = requestAnimationFrame(animate);
}

window.addEventListener("wheel", (e) => {
  e.preventDefault();
  target = Math.max(0, Math.min(maxOffset(), target + e.deltaY * 0.9));
  if (!raf) raf = requestAnimationFrame(animate);
}, { passive: false });

// ===== Right-click context menu (End task — same as the taskbar) =====
function showContextMenu(anchor: HTMLElement, app: TaskbarApp) {
  document.querySelectorAll(".tt-context-menu").forEach((el) => el.remove());

  const menu = document.createElement("div");
  menu.className = "tt-context-menu";

  const endTask = document.createElement("div");
  endTask.className = "tt-context-menu-item danger";
  endTask.textContent = t("tb.endTask");
  endTask.addEventListener("click", () => {
    menu.remove();
    invoke("end_task", { appId: app.id });
    closeStrip();
  });
  menu.appendChild(endTask);

  document.body.appendChild(menu);
  // Window is 260px wide, rail 62 — the menu lives in the transparent zone.
  const rect = anchor.getBoundingClientRect();
  menu.style.left = `74px`;
  menu.style.top = `${Math.max(4, Math.min(window.innerHeight - menu.offsetHeight - 4, rect.top + rect.height / 2 - menu.offsetHeight / 2))}px`;
}

// ===== Dismissal =====
// The strip is persistent: leaving it with the cursor NEVER closes it.
// Esc, an outside click, or activating an app closes it — all with the
// pop-out animation before the window actually hides.
let closing = false;

function closeStrip() {
  if (closing) return;
  closing = true;
  root.classList.remove("shown");
  // Let the pop-out transition (220ms) finish before the backend hides the
  // window (close_table also tears down the outside-click watcher).
  setTimeout(() => invoke("close_table", { name: "taskbar" }), 230);
}

// Backend: click anywhere outside the strip → pop out.
listen("table://taskbar-outside", () => {
  closeStrip();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    document.querySelectorAll(".tt-context-menu").forEach((el) => el.remove());
    closeStrip();
  }
});

// Click outside the context menu closes just the menu.
document.addEventListener("mousedown", (e) => {
  const menu = document.querySelector(".tt-context-menu");
  if (menu && !menu.contains(e.target as Node)) {
    menu.remove();
  }
});

// ===== Listeners + init =====
listen<TaskbarApp[]>("taskbar://apps-updated", (e) => {
  apps = e.payload;
  reconcile();
});

listen("table://taskbar-shown", () => {
  closing = false;
  // Replay the pop-in even when the previous close skipped the animation.
  root.classList.remove("shown");
  void root.offsetWidth;
  root.classList.add("shown");
  // Fresh data — the periodic backend refresh may be up to 2s behind.
  // Reconcile keeps scroll + icons exactly where they are.
  invoke<TaskbarApp[]>("get_taskbar_apps")
    .then((a) => {
      apps = a;
      reconcile();
    })
    .catch(() => {});
});

(async function init() {
  await initI18n();
  await loadInitialTheme();
  try {
    apps = await invoke<TaskbarApp[]>("get_taskbar_apps");
  } catch {}
  reconcile();
})();
