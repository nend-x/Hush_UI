/* =========================================================================
   TABLE 5 — desktop table. The desktop icons in a medium movable window
   (position persists to tables.json). Same icon tiles as the old hushlight
   grid: click launches, shift+click runs as admin, right-click gives the
   Open / Rename / Delete menu, right-click on empty space gives
   New Folder / New File / Refresh.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { applyTheme, type ThemePayload } from "../shared/theme";

interface LauncherItem {
  id: string;
  name: string;
  path: string;
  icon_data_url: string | null;
  is_folder: boolean;
  pinned: boolean;
}

const root = document.getElementById("mt-root")!;
const grid = document.getElementById("td-grid")!;

// Theme + icon-recolor values are applied together by the shared applyTheme
// — startup load AND live theme://changed events both go through it.

listen<ThemePayload>("theme://changed", (e) => {
  applyTheme(e.payload);
});

listen<boolean>("icon-recolor://changed", (e) => {
  document.documentElement.classList.toggle("icon-recolor", e.payload);
});

// ===== Show / hide =====
let closing = false;

listen("table://desktop-shown", () => {
  closing = false;
  launching = false; // a previous session's launch animation must not block new clicks
  // The window is only hidden between sessions, so the DOM persists — clear
  // any stale .launching classes a previous session left behind, or the
  // affected icons would keep spinning on every reopen.
  grid.querySelectorAll(".launcher-item.launching").forEach((el) => {
    el.classList.remove("launching");
  });
  root.classList.remove("shown");
  void root.offsetWidth;
  root.classList.add("shown");
  void refresh();
});

function close() {
  if (closing) return;
  closing = true;
  root.classList.remove("shown");
  // Let the pop-out play before the backend hides the window.
  setTimeout(() => invoke("close_table", { name: "desktop" }), 240);
}

document.getElementById("mt-close")!.addEventListener("click", close);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") close();
});

// ===== Desktop items =====
let items: LauncherItem[] = [];

// While a launch animation is playing the grid ignores further clicks and
// the window closes right after the spin finishes. The launching class is
// dropped when the spin ends (and swept on desktop-shown), so it can never
// stick around across a close/reopen cycle.
const LAUNCH_SPIN_MS = 1000;
let launching = false;

async function refresh() {
  try {
    items = await invoke<LauncherItem[]>("get_desktop_items");
  } catch {
    items = [];
  }
  render();
}

listen<LauncherItem[]>("launcher://items-updated", (e) => {
  items = e.payload;
  render();
});

/// Launch an item and play the 1s spin animation on its icon, then close
/// the desktop table. `el` is the tile that was clicked (null for the
/// context-menu "Open" path when the tile reference is unavailable).
function launchWithSpin(item: LauncherItem, el: HTMLElement | null) {
  if (launching) return;
  launching = true;
  invoke("launch_desktop_item", { itemId: item.id });
  if (el) {
    el.classList.add("launching");
    // Re-trigger the spin even if the tile was re-rendered between clicks.
    void el.offsetWidth;
  }
  // After the 1s spin → drop the launching class (it must never outlive
  // the spin, the window is only hidden and its DOM persists) → close the
  // desktop table (its own pop-out plays before the backend hides it).
  setTimeout(() => {
    el?.classList.remove("launching");
    close();
  }, LAUNCH_SPIN_MS);
}

// ===== Keyed reconciliation (0.3.0) =====
// The old render() wiped grid.innerHTML and rebuilt EVERY tile on every
// launcher://items-updated — so each update replayed the item-fade-in
// animation on all icons (the "icons moving in a weird way" bug), dropped
// hover state, and stuttered on larger desktops. Tiles are now keyed by
// item id: existing tiles update in place, new tiles append (and animate
// exactly once), gone tiles are removed, and DOM order is only adjusted
// when the scan order actually changed.
const tiles = new Map<string, HTMLElement>();
let emptyEl: HTMLElement | null = null;

function buildTile(item: LauncherItem): HTMLElement {
  const el = document.createElement("div");
  el.className = "launcher-item";
  el.dataset.id = item.id;

  const iconBox = document.createElement("div");
  iconBox.className = "icon-box";

  const label = document.createElement("div");
  label.className = "label";
  el.appendChild(iconBox);
  el.appendChild(label);

  el.addEventListener("click", (e) => {
    if (launching) return;
    const current = items.find((i) => i.id === el.dataset.id);
    if (!current) return;
    if (e.shiftKey) {
      invoke("execute_run_admin", { command: current.path });
    } else {
      launchWithSpin(current, el);
    }
  });

  el.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    const current = items.find((i) => i.id === el.dataset.id);
    if (current) showItemContextMenu(e.clientX, e.clientY, current, el);
  });

  return el;
}

function updateTile(el: HTMLElement, item: LauncherItem) {
  // Pin marker — cheap class toggle, keeps the terracotta dot in sync
  // without touching the icon/label reconciliation below.
  el.classList.toggle("pinned", item.pinned);

  // Label — update only when changed (avoid layout churn).
  const label = el.querySelector(".label") as HTMLElement;
  if (label.textContent !== item.name) {
    label.textContent = item.name;
    el.title = item.name;
  }

  // Icon — compare the data URL currently rendered against the new one.
  const iconBox = el.querySelector(".icon-box") as HTMLElement;
  const img = iconBox.querySelector("img");
  const fb = iconBox.querySelector("span");
  if (item.icon_data_url) {
    if (img && img.src === item.icon_data_url) return;
    iconBox.innerHTML = "";
    const newImg = document.createElement("img");
    newImg.src = item.icon_data_url;
    newImg.alt = item.name;
    newImg.draggable = false;
    iconBox.appendChild(newImg);
  } else {
    const fallback = item.is_folder ? "▤" : (item.name || "?")[0].toUpperCase();
    if (fb && fb.textContent === fallback) return;
    iconBox.innerHTML = "";
    const newFb = document.createElement("span");
    newFb.textContent = fallback;
    newFb.style.cssText = "font-family:var(--font-display);font-size:16px;color:var(--sand);";
    iconBox.appendChild(newFb);
  }
}

function render() {
  const nextIds = new Set(items.map((i) => i.id));

  // Remove tiles whose items are gone.
  for (const [id, el] of tiles) {
    if (!nextIds.has(id)) {
      el.remove();
      tiles.delete(id);
    }
  }

  // Update or create tiles; keep DOM order in sync with the scan order
  // (pins first in pin order, then folders, then alphabetical — the
  // backend's deterministic sort).
  let prev: HTMLElement | null = null;
  for (const item of items) {
    let el = tiles.get(item.id);
    if (el) {
      updateTile(el, item);
    } else {
      el = buildTile(item);
      updateTile(el, item);
      tiles.set(item.id, el);
    }
    // insertBefore(el, null) == appendChild, so this also appends new
    // tiles — and only MOVES existing tiles when the order changed.
    if (el.previousElementSibling !== prev) {
      grid.insertBefore(el, prev ? prev.nextElementSibling : grid.firstElementChild);
    }
    prev = el;
  }

  // Empty state.
  if (items.length === 0) {
    if (!emptyEl) {
      emptyEl = document.createElement("div");
      emptyEl.className = "td-empty";
      emptyEl.textContent = "Desktop folder is empty";
    }
    if (!emptyEl.isConnected) grid.appendChild(emptyEl);
  } else if (emptyEl?.isConnected) {
    emptyEl.remove();
  }
}

// ===== Context menus =====
grid.addEventListener("contextmenu", (e) => {
  if (e.target === grid) {
    e.preventDefault();
    showBackgroundContextMenu(e.clientX, e.clientY);
  }
});

function showItemContextMenu(x: number, y: number, item: LauncherItem, tile: HTMLElement | null) {
  document.querySelectorAll(".context-menu").forEach((el) => el.remove());

  const menu = document.createElement("div");
  menu.className = "context-menu";
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;

  const open = document.createElement("div");
  open.className = "context-menu-item";
  open.textContent = "Open";
  open.addEventListener("click", () => {
    menu.remove();
    launchWithSpin(item, tile);
  });
  menu.appendChild(open);

  // Pin to top / Unpin — pinned items float to the start of the grid
  // (pin order), everything else keeps the deterministic sort.
  const pin = document.createElement("div");
  pin.className = "context-menu-item";
  pin.textContent = item.pinned ? "Unpin" : "Pin to top";
  pin.addEventListener("click", () => {
    menu.remove();
    invoke("set_desktop_item_pinned", { itemId: item.id, pinned: !item.pinned });
  });
  menu.appendChild(pin);

  const sep1 = document.createElement("div");
  sep1.className = "context-menu-separator";
  menu.appendChild(sep1);

  const rename = document.createElement("div");
  rename.className = "context-menu-item";
  rename.textContent = "Rename";
  rename.addEventListener("click", () => {
    menu.remove();
    showRenameDialog(item);
  });
  menu.appendChild(rename);

  const del = document.createElement("div");
  del.className = "context-menu-item danger";
  del.textContent = "Delete";
  del.addEventListener("click", () => {
    menu.remove();
    invoke("delete_desktop_item", { itemId: item.id });
  });
  menu.appendChild(del);

  document.body.appendChild(menu);
  clampMenu(menu, x, y);
}

function showBackgroundContextMenu(x: number, y: number) {
  document.querySelectorAll(".context-menu").forEach((el) => el.remove());

  const menu = document.createElement("div");
  menu.className = "context-menu";
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;

  const newItem = document.createElement("div");
  newItem.className = "context-menu-item";
  newItem.innerHTML = `<span>New</span><span class="context-menu-submenu-arrow">▸</span>`;

  const submenu = document.createElement("div");
  submenu.className = "context-menu-submenu";

  const folderItem = document.createElement("div");
  folderItem.className = "context-menu-item";
  folderItem.textContent = "Folder";
  folderItem.addEventListener("click", () => {
    menu.remove();
    showDialog("New folder", "", "Folder name", (name) => {
      invoke("create_desktop_item", { name, isFolder: true });
    });
  });

  const fileItem = document.createElement("div");
  fileItem.className = "context-menu-item";
  fileItem.textContent = "File";
  fileItem.addEventListener("click", () => {
    menu.remove();
    showDialog("New file", "", "name.extension", (name) => {
      invoke("create_desktop_item", { name, isFolder: false });
    });
  });

  submenu.appendChild(folderItem);
  submenu.appendChild(fileItem);
  newItem.appendChild(submenu);
  menu.appendChild(newItem);

  const sep = document.createElement("div");
  sep.className = "context-menu-separator";
  menu.appendChild(sep);

  const refreshItem = document.createElement("div");
  refreshItem.className = "context-menu-item";
  refreshItem.textContent = "Refresh";
  refreshItem.addEventListener("click", () => {
    menu.remove();
    invoke("refresh_desktop");
  });
  menu.appendChild(refreshItem);

  document.body.appendChild(menu);
  clampMenu(menu, x, y);
}

function clampMenu(menu: HTMLElement, x: number, y: number) {
  const rect = menu.getBoundingClientRect();
  if (rect.right > window.innerWidth - 8) menu.style.left = `${x - rect.width}px`;
  if (rect.bottom > window.innerHeight - 8) menu.style.top = `${y - rect.height}px`;
}

document.addEventListener("mousedown", (e) => {
  document.querySelectorAll(".context-menu").forEach((el) => {
    if (!el.contains(e.target as Node)) el.remove();
  });
});

// ===== Rename dialog (same as the launcher's modal) =====
function showRenameDialog(item: LauncherItem) {
  showDialog("Rename", item.name, "New name", (name) => {
    invoke("rename_desktop_item", { itemId: item.id, newName: name });
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

  const actions = document.createElement("div");
  actions.className = "modal-actions";

  const cancel = document.createElement("button");
  cancel.className = "modal-btn";
  cancel.textContent = "Cancel";

  const confirm = document.createElement("button");
  confirm.className = "modal-btn primary";
  confirm.textContent = "OK";

  actions.appendChild(cancel);
  actions.appendChild(confirm);
  dialog.appendChild(titleEl);
  dialog.appendChild(input);
  dialog.appendChild(actions);
  backdrop.appendChild(dialog);
  document.body.appendChild(backdrop);

  setTimeout(() => input.focus(), 10);

  const closeDialog = () => backdrop.remove();
  cancel.addEventListener("click", closeDialog);
  backdrop.addEventListener("click", (e) => {
    if (e.target === backdrop) closeDialog();
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      const v = input.value.trim();
      if (v) {
        onConfirm(v);
        closeDialog();
      }
    } else if (e.key === "Escape") {
      closeDialog();
    }
  });
  confirm.addEventListener("click", () => {
    const v = input.value.trim();
    if (v) {
      onConfirm(v);
      closeDialog();
    }
  });
}

// ===== Init =====
(async function init() {
  try {
    const theme = await invoke<ThemePayload | null>("get_active_theme");
    if (theme) applyTheme(theme);
    if (await invoke<boolean>("load_icon_recolor").catch(() => false)) {
      document.documentElement.classList.add("icon-recolor");
    }
  } catch {}
  await refresh();
})();
