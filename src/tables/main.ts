/* =========================================================================
   TABLES picker — the PIE menu that appears while the Win key is held.
   The backend shows this window on a Win hold and reads our hover report
   (set_tables_hover) when the user releases Win over a slice.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { applyTheme, type ThemePayload } from "../shared/theme";
import { initI18n, t } from "../shared/i18n";

interface ShowPayload {
  x: number;
  y: number;
  center: boolean;
  // Picker generation echoed back by tables_rendered — lets the backend
  // reject render confirmations that belong to an older show.
  seq: number;
}

// Theme + icon-recolor values are applied together by the shared applyTheme
// — the picker must follow the active theme (startup load AND live events).

const root = document.getElementById("tables-root")!;
const slices = Array.from(document.querySelectorAll<SVGGElement>(".pie-slice"));
const hubClock = document.getElementById("pie-clock") as unknown as SVGTextElement;

let hoverTask: number | null = null;
// Pending animated-hide blanking timer — cancelled when a new show arrives,
// so a stale hide can never blank a freshly re-opened picker.
let hideBlankTimer: number | null = null;

// ===== Pie geometry ======================================================
// Five equal 72° wedges radiating from the anchor point, clockwise from
// the top: hushlight, desktop, taskbar, widgets, settings. A small gap
// between wedges reads as slice borders and keeps the center hub visible.

const SLICE_ORDER = ["hushlight", "desktop", "taskbar", "widgets", "settings"];
const RADIUS = 138;      // outer wedge radius (px)
const SLICE_DEG = 360 / SLICE_ORDER.length; // no angular gap — wedges share edges like a real pie
const ICON_R = 86;       // icon center distance from anchor
const HUB_R = 26;        // center hub radius

const TABLE_IDS: Record<string, number> = {
  taskbar: 1,
  settings: 2,
  widgets: 3,
  hushlight: 4,
  desktop: 5,
};

const polar = (cx: number, cy: number, r: number, deg: number): [number, number] => {
  const rad = (deg * Math.PI) / 180;
  return [cx + r * Math.cos(rad), cy + r * Math.sin(rad)];
};

// Lay out every wedge (and its icon/label) around one anchor point.
function layoutPie(x: number, y: number) {
  const hub = document.getElementById("pie-hub")! as unknown as SVGCircleElement;
  hub.setAttribute("cx", `${x}`);
  hub.setAttribute("cy", `${y}`);
  hub.setAttribute("r", `${HUB_R}`);

  // Hub clock rides the hub circle (dominant-baseline centers it vertically).
  hubClock.setAttribute("x", `${x}`);
  hubClock.setAttribute("y", `${y}`);

  for (const slice of slices) {
    const i = SLICE_ORDER.indexOf(slice.dataset.table!);
    const mid = -90 + i * SLICE_DEG; // slice i's mid-angle, clockwise from up
    const a0 = mid - SLICE_DEG / 2;
    const a1 = mid + SLICE_DEG / 2;
    const [x0, y0] = polar(x, y, RADIUS, a0);
    const [x1, y1] = polar(x, y, RADIUS, a1);

    const wedge = slice.querySelector<SVGPathElement>(".pie-wedge")!;
    wedge.setAttribute("d",
      `M ${x} ${y} L ${x0.toFixed(2)} ${y0.toFixed(2)} ` +
      `A ${RADIUS} ${RADIUS} 0 0 1 ${x1.toFixed(2)} ${y1.toFixed(2)} Z`);

    // The icon sits in a translated <g>; the scale animation lives on the
    // nested <g>/<svg> INSIDE it, so the scale origin is the icon center —
    // CSS transform-box: fill-box is unreliable on nested <svg> in WebView2
    // and made icons fly from the SVG's top-left corner.
    const [ix, iy] = polar(x, y, ICON_R, mid);
    const anchor = slice.querySelector<SVGGElement>(".pie-icon")!;
    anchor.setAttribute("transform", `translate(${ix.toFixed(2)} ${iy.toFixed(2)})`);
    const icon = anchor.querySelector<SVGSVGElement>("svg")!;
    icon.setAttribute("x", "-12");
    icon.setAttribute("y", "-12");
    icon.setAttribute("width", "24");
    icon.setAttribute("height", "24");

  }
}

function reportHover(id: number) {
  if (hoverTask) window.clearTimeout(hoverTask);
  // Micro-defer: a quick pass over a slice between two neighbors should
  // never leave a stale hover report racing the hook's Win-up read.
  hoverTask = window.setTimeout(() => {
    invoke("set_tables_hover", { id });
  }, 8);
}

// Hover tracking — the whole release gesture depends on this staying fresh.
for (const slice of slices) {
  slice.addEventListener("mouseenter", () => {
    slices.forEach((s) => s.classList.remove("hover"));
    slice.classList.add("hover");
    reportHover(TABLE_IDS[slice.dataset.table!] ?? 0);
  });
  slice.addEventListener("mouseleave", () => {
    slice.classList.remove("hover");
    reportHover(0);
  });
  // Click fallback (the primary gesture is hover + release Win) — e.g. a
  // user who holds Win forever and just clicks instead.
  slice.addEventListener("click", (e) => {
    e.stopPropagation();
    invoke("open_table", { name: slice.dataset.table });
  });
}

// Click anywhere else on the overlay → dismiss.
root.addEventListener("mousedown", () => {
  invoke("close_table", { name: "tables" });
});

// ===== Hub clock =====
// The (formerly empty) center hub shows the local time. Format follows
// the settings' clock format (clock_24h — same segmented control that
// drives the taskbar clock). While the pie is shown the clock ticks
// every second; the interval dies with the picker so the hidden window
// never wakes its (suspended) webview for nothing.
let clock24h = true;
// Pie hub clock toggle (settings). Read fresh on every show — the pie is
// suspended while hidden, so a settings change while closed is picked up
// on the next open without any live-update machinery.
let pieClockOn = true;
let clockTimer: number | null = null;

function formatNow(): string {
  const now = new Date();
  const h = now.getHours();
  const m = now.getMinutes().toString().padStart(2, "0");
  if (clock24h) return `${h.toString().padStart(2, "0")}:${m}`;
  const h12 = h % 12 === 0 ? 12 : h % 12;
  return `${h12}:${m} ${h < 12 ? "AM" : "PM"}`;
}

// Perfect fit: the text must stay inside the hub circle. Measure after
// each render and shrink when the current format is wider (the 12h
// variant with its AM/PM suffix); never grow past the base size.
const HUB_BASE_FONT = 14;
const HUB_MAX_TEXT_W = HUB_R * 1.7;

function renderHubClock() {
  hubClock.textContent = formatNow();
  hubClock.style.fontSize = `${HUB_BASE_FONT}px`;
  const w = hubClock.getComputedTextLength();
  if (w > HUB_MAX_TEXT_W) {
    hubClock.style.fontSize =
      `${Math.max(8, Math.floor((HUB_BASE_FONT * HUB_MAX_TEXT_W) / w))}px`;
  }
}

function startHubClock() {
  renderHubClock();
  if (clockTimer === null) clockTimer = window.setInterval(renderHubClock, 1000);
}

function stopHubClock() {
  if (clockTimer !== null) {
    window.clearInterval(clockTimer);
    clockTimer = null;
  }
}

async function loadClockFormat() {
  try {
    const s = await invoke<{ clock_24h?: boolean; pie_clock?: boolean }>("load_settings");
    clock24h = s.clock_24h ?? true;
    pieClockOn = s.pie_clock ?? true;
  } catch {}
}

listen<ShowPayload>("tables://show", (e) => {
  const { x, y, seq } = e.payload;
  // Cancel any pending hide-path blanking (rapid hide → re-open): the show
  // generation owns the surface now; a late hide timer must not blank it.
  if (hideBlankTimer !== null) {
    window.clearTimeout(hideBlankTimer);
    hideBlankTimer = null;
  }
  // Undo the hide-path blanking FIRST — the pie must be back in the render
  // tree for this frame's layout and for the pop-in transition to play.
  root.style.visibility = "";
  // Anchor point → CSS vars for the vignette + the pie layout.
  root.style.setProperty("--pick-x", `${x}px`);
  root.style.setProperty("--pick-y", `${y}px`);
  layoutPie(x, y);
  root.classList.remove("shown");
  // Force a reflow so the pop-in transition replays every open.
  void root.offsetWidth;
  root.classList.add("shown");
  // 0.3.3 render handshake: the backend resumes our (suspended) webview
  // controller while the window is still cloaked, emits this event, and
  // only uncloaks once we confirm the pie is laid out at the NEW anchor and
  // a frame is committed. That is what kills the "pie spawns at its old
  // dismissal spot and teleports to the cursor" microframe glitch — the
  // stale frame from the previous open is never on screen. Double-rAF = the
  // next painted frame includes today's layout; the setTimeout is a belt-
  // and-braces confirmation in case the rAF pipeline is still spinning up
  // right after the controller resume (the backend wait is bounded anyway).
  // 0.4.0: the backend ALSO blanks the picker before every suspend (hide
  // handshake), so the re-presented "stale frame" is now blank regardless —
  // this handshake remains as the latency optimization (reveal as soon as
  // the correct frame exists), no longer as the correctness guarantee.
  const confirmRendered = () => invoke("tables_rendered", { seq });
  requestAnimationFrame(() => requestAnimationFrame(confirmRendered));
  window.setTimeout(confirmRendered, 120);
  // Tick from the moment the pie pops in; re-pull format + clock toggle
  // every show so a settings change while the pie was closed is picked up
  // without a restart.
  void loadClockFormat().then(() => {
    hubClock.style.display = pieClockOn ? "" : "none";
    if (pieClockOn) {
      renderHubClock();
      startHubClock();
    } else {
      stopHubClock();
    }
  });
});

listen<ThemePayload>("theme://changed", (e) => {
  applyTheme(e.payload);
});
listen<boolean>("icon-recolor://changed", (e) => {
  document.documentElement.classList.toggle("icon-recolor", e.payload);
});

interface HidePayload {
  // Picker generation the backend assigned to this hide — echoed back by
  // tables_hidden so a stale confirmation can never satisfy a newer hide.
  seq: number;
  // true = instant dismiss (a table is opening): blank the root in THIS
  // frame. false = release-dismiss: let the pop-out play first.
  instant: boolean;
}

listen<HidePayload>("tables://hide", (e) => {
  const { seq, instant } = e.payload;
  root.classList.remove("shown");
  slices.forEach((s) => s.classList.remove("hover"));
  stopHubClock();

  // 0.4.0 hide handshake — the critical half of the flash fix. The backend
  // suspends the picker's WebView2 controller right after this confirm:
  // whatever frame the surface presented LAST is what the next open's
  // controller resume re-presents. If we suspended while the pie was still
  // rendered, the next open would show the pie at the OLD anchor for a
  // frame before the (raced) show handshake could move it — the residual
  // spawn-position flash. So: blank the root (visibility:hidden keeps
  // layout — the hub clock's getComputedTextLength still works — but
  // removes every pixel from the next presented frame) and only tell the
  // backend once a blank frame is actually committed (double-rAF).
  const confirmHidden = () => invoke("tables_hidden", { seq });
  const blankAndConfirm = () => {
    root.style.visibility = "hidden";
    requestAnimationFrame(() => requestAnimationFrame(confirmHidden));
  };
  if (instant) {
    // Table chosen — blank in THIS frame (the opened table provides the
    // visual transition); the backend cloak+suspend lands ~2 frames later.
    blankAndConfirm();
  } else {
    // Release-dismiss — let the pop-out play (~260ms incl. stagger), then
    // blank + confirm. The backend won't cloak before its 280ms floor
    // anyway, and cancels this entirely if a new show arrives first.
    hideBlankTimer = window.setTimeout(blankAndConfirm, 260);
  }
});

// Apply the active theme + recolor state at startup (the picker loads
// hidden; by the time it's first shown the vars are already set).
(async function init() {
  await initI18n();
  try {
    const theme = await invoke<ThemePayload | null>("get_active_theme");
    if (theme) applyTheme(theme);
    if (await invoke<boolean>("load_icon_recolor").catch(() => false)) {
      document.documentElement.classList.add("icon-recolor");
    }
  } catch {}
})();
