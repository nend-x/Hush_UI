/* =========================================================================
   Per-app volume dropdown for the audio widget.
   Shared by the widgets table and the launcher's embedded widget panel —
   both windows carry the same #audio-widget markup (master row + the
   dropdown markup this module wires up). The backend enumerates WASAPI
   sessions of the default render device (get_app_volumes) and applies
   volume per process (set_app_volume), so the list is "the Windows volume
   mixer, shrunk": one row per app that owns an audio session right now.

   The list refreshes on a 2s loop ONLY while the dropdown is open — closed
   state costs zero IPC (same idle philosophy as the 0.3.0 polling fix).
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";

interface AppVolume {
  pid: number;
  name: string;
  volume: number;
  icon: string | null;
}

let initialized = false;
let open = false;
let refreshTimer: number | null = null;
// While a slider is being dragged the refresh loop must not touch its row
// (the thumb would jump under the user's pointer). pid of the slider
// currently being interacted with, or null.
let draggingPid: number | null = null;
// Per-pid set debouncers (mirrors the 50ms master-slider debounce).
const pendingSets: Record<number, number> = {};

export function initAudioApps(i18nPrefix: "wg" | "ln"): void {
  if (initialized) return;
  const toggle = document.getElementById("audio-apps-toggle");
  const list = document.getElementById("audio-apps-list");
  if (!toggle || !list) return;
  initialized = true;

  toggle.addEventListener("click", () => {
    open = !open;
    toggle.classList.toggle("open", open);
    toggle.setAttribute("aria-expanded", String(open));
    list.hidden = !open;
    if (open) {
      void refresh(i18nPrefix);
      if (refreshTimer === null) {
        refreshTimer = window.setInterval(() => void refresh(i18nPrefix), 2000);
      }
    } else if (refreshTimer !== null) {
      window.clearInterval(refreshTimer);
      refreshTimer = null;
    }
  });

  // Language switch while an empty list is on screen: re-render the label.
  document.addEventListener("i18n:changed", () => {
    if (open && list.childElementCount === 0) showEmpty(list, i18nPrefix);
  });
}

function showEmpty(list: HTMLElement, prefix: "wg" | "ln"): void {
  list.innerHTML = "";
  const empty = document.createElement("div");
  empty.className = "audio-apps-empty";
  empty.textContent = t(`${prefix}.noApps`);
  list.appendChild(empty);
}

async function refresh(prefix: "wg" | "ln"): Promise<void> {
  const list = document.getElementById("audio-apps-list");
  if (!list || !open) return;
  let apps: AppVolume[] = [];
  try {
    apps = await invoke<AppVolume[]>("get_app_volumes");
  } catch {}
  if (!open) return; // closed while the fetch was in flight

  // Surgical diff: update existing rows in place, drop dead pids, append
  // new ones — a full innerHTML rebuild every 2s would fight any slider
  // the user is touching and kill hover states.
  const remaining = new Map<number, AppVolume>(apps.map((a) => [a.pid, a]));
  for (const row of Array.from(list.querySelectorAll<HTMLElement>(".audio-app-row"))) {
    const pid = Number(row.dataset.pid);
    const app = remaining.get(pid);
    if (!app) {
      row.remove();
      continue;
    }
    remaining.delete(pid);
    const pct = Math.round(app.volume * 100);
    if (draggingPid !== pid) {
      const slider = row.querySelector<HTMLInputElement>(".audio-app-slider");
      const val = row.querySelector<HTMLElement>(".audio-app-val");
      if (slider) slider.value = String(pct);
      if (val) val.textContent = `${pct}%`;
    }
    const nameEl = row.querySelector<HTMLElement>(".audio-app-name");
    if (nameEl && nameEl.textContent !== app.name) {
      nameEl.textContent = app.name;
      nameEl.title = app.name;
    }
  }
  for (const app of remaining.values()) list.appendChild(buildRow(app));

  const emptyEl = list.querySelector(".audio-apps-empty");
  if (list.childElementCount === 0) {
    if (!emptyEl) showEmpty(list, prefix);
  } else if (emptyEl) {
    emptyEl.remove();
  }
}

function buildRow(app: AppVolume): HTMLElement {
  const row = document.createElement("div");
  row.className = "audio-app-row";
  row.dataset.pid = String(app.pid);

  const icon = document.createElement(app.icon ? "img" : "span");
  icon.className = "audio-app-icon";
  if (app.icon) (icon as HTMLImageElement).src = app.icon;
  else icon.textContent = (app.name || "?")[0].toUpperCase();
  row.appendChild(icon);

  const name = document.createElement("span");
  name.className = "audio-app-name";
  name.textContent = app.name;
  name.title = app.name;
  row.appendChild(name);

  const slider = document.createElement("input");
  slider.type = "range";
  slider.className = "audio-slider audio-app-slider";
  slider.min = "0";
  slider.max = "100";
  slider.value = String(Math.round(app.volume * 100));

  const val = document.createElement("span");
  val.className = "audio-val audio-app-val";
  val.textContent = `${slider.value}%`;

  slider.addEventListener("pointerdown", () => {
    draggingPid = app.pid;
  });
  slider.addEventListener("input", () => {
    val.textContent = `${slider.value}%`;
    if (pendingSets[app.pid] !== undefined) window.clearTimeout(pendingSets[app.pid]);
    pendingSets[app.pid] = window.setTimeout(() => {
      delete pendingSets[app.pid];
      invoke("set_app_volume", { pid: app.pid, volume: parseInt(slider.value, 10) / 100 });
    }, 50);
  });
  const release = () => {
    if (draggingPid === app.pid) draggingPid = null;
  };
  slider.addEventListener("pointerup", release);
  slider.addEventListener("pointercancel", release);
  slider.addEventListener("change", release);

  row.appendChild(slider);
  row.appendChild(val);
  return row;
}
