/* =========================================================================
   Hush_UI — First-run tutorial (frontend)
   A small looping animation teaches the Win-hold pie gesture:

     1. cursor + Windows key shown
     2. Win key press/hold animation → fake pie menu pops in → the cursor
        glides to the settings slice and hovers it
     3. Win release (unhold animation)
     4. the fake settings window appears — hold, then the loop restarts

   Pure frontend choreography; the backend only shows/hides this window
   (tutorial://start on show, finish_tutorial on "Understood").
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { applyTheme, type ThemePayload } from "../shared/theme";
import { initI18n, t } from "../shared/i18n";

// ===== Demo geometry (same wedge math as the real pie picker) ============
const SLICE_ORDER = ["hushlight", "desktop", "taskbar", "widgets", "settings"];
const RADIUS = 118;
const SLICE_DEG = 360 / SLICE_ORDER.length;
const ICON_R = 74;
const HUB_R = 20;
const SVG_NS = "http://www.w3.org/2000/svg";

const polar = (cx: number, cy: number, r: number, deg: number): [number, number] => {
  const rad = (deg * Math.PI) / 180;
  return [cx + r * Math.cos(rad), cy + r * Math.sin(rad)];
};

const stage = document.querySelector(".tut-stage") as HTMLElement;
// SAFETY: the #tut-pie element is an <svg> in index.html; the DOM lib still
// types getElementById as HTMLElement for HTML ids, so narrow explicitly.
const pie = document.getElementById("tut-pie") as unknown as SVGSVGElement;
const pieG = document.getElementById("tut-pie-g")!;
// SAFETY: #tut-pie-hub is a <circle> in index.html (same narrowing caveat).
const hub = document.getElementById("tut-pie-hub") as unknown as SVGCircleElement;
const fakewin = document.getElementById("tut-fakewin")!;
const winkey = document.getElementById("tut-winkey")!;
const fill = document.getElementById("tut-winkey-fill")!;
const cursor = document.getElementById("tut-cursor")!;
const btn = document.getElementById("tut-btn")!;

// Minimal stroke icons — same style as the real picker.
const ICONS: { [table: string]: string } = {
  hushlight: '<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>',
  desktop: '<rect x="3" y="4" width="18" height="13" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/>',
  taskbar: '<rect x="3" y="14" width="18" height="7" rx="2"/><line x1="8" y1="17.5" x2="8" y2="17.5"/>',
  widgets: '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>',
};

function buildPie() {
  hub.setAttribute("cx", "160");
  hub.setAttribute("cy", "160");
  hub.setAttribute("r", String(HUB_R));
  // SAFETY: the innerHTML below is built ONLY from the static ICONS table
  // and fixed numbers — no user-controlled string ever reaches it.
  pieG.innerHTML = "";
  for (const table of SLICE_ORDER) {
    const i = SLICE_ORDER.indexOf(table);
    const mid = -90 + i * SLICE_DEG;
    const a0 = mid - SLICE_DEG / 2;
    const a1 = mid + SLICE_DEG / 2;
    const [x0, y0] = polar(160, 160, RADIUS, a0);
    const [x1, y1] = polar(160, 160, RADIUS, a1);
    const [ix, iy] = polar(160, 160, ICON_R, mid);
    const g = document.createElementNS(SVG_NS, "g");
    g.classList.add("tut-pie-slice");
    g.dataset.table = table;
    g.innerHTML =
      `<path class="tut-pie-wedge" d="M 160 160 L ${x0.toFixed(2)} ${y0.toFixed(2)} ` +
      `A ${RADIUS} ${RADIUS} 0 0 1 ${x1.toFixed(2)} ${y1.toFixed(2)} Z"/>` +
      `<g class="tut-pie-icon"><svg x="${(ix - 11).toFixed(2)}" y="${(iy - 11).toFixed(2)}" width="22" height="22" viewBox="0 0 24 24">${ICONS[table]}</svg></g>`;
    pieG.appendChild(g);
  }
}

// ===== Timeline ==========================================================
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

// Move the cursor so its TIP lands exactly on (x, y). The arrow tip sits at
// ~(5,3)/24 of the 22px box, so offset the element by that amount.
function moveCursor(x: number, y: number, ms: number) {
  const t = `left ${ms}ms cubic-bezier(0.32,0.72,0.45,1), top ${ms}ms cubic-bezier(0.32,0.72,0.45,1)`;
  cursor.style.transition = t;
  cursor.style.left = `${x - (5 / 24) * 22}px`;
  cursor.style.top = `${y - (3 / 24) * 22}px`;
}

function setSliceHover(on: boolean) {
  pieG.querySelector(".tut-pie-slice[data-table='settings']")?.classList.toggle("hover", on);
}

let running = false;

async function runLoop() {
  if (running) return;
  running = true;

  // Stage center = the pie anchor (window is fixed-size, so this is stable).
  const r = stage.getBoundingClientRect();
  const cx = r.width / 2;
  const cy = r.height * 0.45;
  pie.style.left = `${cx - 160}px`;
  pie.style.top = `${cy - 160}px`;
  pie.style.margin = "0";
  buildPie();

  // Reset the scene (cursor parked bottom-right, everything hidden).
  pie.classList.remove("shown");
  fakewin.classList.remove("shown");
  winkey.classList.remove("shown", "pressed");
  fill.style.transition = "none";
  fill.style.transform = "scaleX(0)";
  cursor.style.transition = "none";
  cursor.style.left = `${cx + 150}px`;
  cursor.style.top = `${cy + 170}px`;
  cursor.getBoundingClientRect(); // flush styles before animating

  await sleep(600); // beat 1 — cursor + Win key visible
  winkey.classList.add("shown");

  // beat 2 — the Win key presses itself (keyboard gesture, no cursor): a
  // quick press flash, then the hold-fill runs while Win is held.
  winkey.classList.add("pressed");
  await sleep(280);
  fill.style.transition = "transform 600ms linear";
  fill.style.transform = "scaleX(1)";
  await sleep(700);

  // pie pops in around the cursor while Win is still held
  pie.classList.add("shown");
  await sleep(450);
  // cursor glides to the settings slice. Settings is the 5th wedge (i=4):
  // mid-angle = -90 + 4*72 = 198°  (≡ -162°) — exactly up-left. Aim at the
  // icon point (ICON_R from center), not an arbitrary radius, and stop
  // between hub and icon so the tip is clearly inside the wedge.
  const SETTINGS_MID = -90 + 4 * SLICE_DEG; // 198°
  const [sx, sy] = polar(cx, cy, (ICON_R + HUB_R) / 2 + 12, SETTINGS_MID);
  moveCursor(sx, sy, 750);
  await sleep(850);
  setSliceHover(true);
  await sleep(450);

  // beat 3 — Win release while the settings slice is hovered: the unhold
  // animation plays (fill drains right→left, key pops back), the slice
  // highlight drops and the pie dismisses.
  winkey.classList.remove("pressed");
  fill.style.transition = "transform 320ms cubic-bezier(0.32,0.72,0.45,1)";
  fill.style.transform = "scaleX(0)";
  await sleep(280);
  setSliceHover(false);
  pie.classList.remove("shown");

  // beat 4 — the settings window opens as a result of the release
  await sleep(260);
  fakewin.classList.add("shown");
  await sleep(1800);

  // reset — then the loop restarts
  fakewin.classList.remove("shown");
  await sleep(500);
  setSliceHover(false);
  await sleep(300);
  running = false;
  void runLoop();
}

// ===== Backend wiring ====================================================
listen("tutorial://start", () => {
  void runLoop();
});

listen<ThemePayload>("theme://changed", (e) => {
  applyTheme(e.payload);
});

btn.addEventListener("click", () => {
  invoke("finish_tutorial");
});

// Apply the active theme at load so the demo matches the current look.
(async function init() {
  await initI18n();
  try {
    const theme = await invoke<ThemePayload | null>("get_active_theme");
    if (theme) applyTheme(theme);
  } catch {
    // theme read is best-effort; the CSS defaults already match dark
  }
  void runLoop();
})();
