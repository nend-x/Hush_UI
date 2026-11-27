/* =========================================================================
   Slider fill painting — gives range inputs a filled track (the portion
   between min and the current value, painted with the accent color up to
   --fill and the track color after it). A native-feeling slider shows
   its value; the flat range input otherwise looks identical at 20% and
   at 80%.

   The fill is a CSS custom property consumed by a linear-gradient in
   quickshell.css ([data-theme="quickshell-dark"] slider rules), so any
   engine without the helper simply renders the old flat track.

   Painting is event-driven:
   - installSliderFillPainting(): one delegated `input` listener covers
     every slider on the page, including dynamically created ones (the
     per-app volume rows).
   - paintSliderFill(): explicit calls after PROGRAMMATIC value writes
     (loaders, refresh loops) — no `input` event fires for those, and a
     stale fill is exactly the kind of "almost right" detail that reads
     as sloppy.
   ========================================================================= */

/// Paint the fill for one slider from its current value.
export function paintSliderFill(el: HTMLInputElement): void {
  const min = Number(el.min) || 0;
  const max = Number(el.max) || 100;
  const value = Number(el.value);
  const pct = max > min ? ((value - min) / (max - min)) * 100 : 0;
  el.style.setProperty("--fill", pct.toFixed(1) + "%");
}

/// Install the page-wide delegated painter. Call once per page init —
/// covers user interaction on every range input, existing or future.
export function installSliderFillPainting(): void {
  document.addEventListener(
    "input",
    (e) => {
      const t = e.target;
      if (t instanceof HTMLInputElement && t.type === "range") {
        paintSliderFill(t);
      }
    },
    true,
  );
}
