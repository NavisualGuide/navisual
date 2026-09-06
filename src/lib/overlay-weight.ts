/**
 * Pointer thickness — the one weight multiplier for every stroke the screen guide draws.
 *
 * Shared because two separate entry points need the same mapping: Overlay.svelte applies
 * it to the canvas, and App.svelte shows the resulting multiplier next to the slider so
 * the setting reads as what it is. Keeping one copy also means this whole feature can be
 * retired by deleting one file and its two call sites, once a good fixed value is known.
 *
 * WHY A MULTIPLIER, NOT A WIDTH. The stroke widths in Overlay.svelte are hand-tuned
 * RATIOS, not free numbers: a bracket's 5.5 shadow reads as a dark edge under its 3.0
 * accent, ripples taper 2.5 -> 0.7 as they fade, the crosshair stays hairline against
 * both. Scaling everything by one factor preserves those relationships; letting a setting
 * name a literal width would flatten them.
 *
 * WHY PIECEWISE. A plain thickness / 4 spans 0.25x-2.5x, and 0.25x puts the brackets at
 * 0.75 px — invisible, which defeats a visibility control. This maps every slider position
 * to something you can actually see, with the default landing on an exact no-op:
 *
 *     1 -> 0.60x     4 -> 1.00x (default)     7 -> 1.50x     10 -> 2.00x
 *
 * Only STROKE WIDTHS scale. Geometry — bracket arm length, beacon radius, ripple growth,
 * crosshair reach — is layout, not weight, and stays put.
 *
 * NOTE ON HIGH-DPI. This is independent of display scaling. Every pointer draw function
 * already divides by the canvas DPR and scales back (see `dprOf` in Overlay.svelte), so a
 * given multiplier is the same PERCEIVED weight at 100% and at 200%. If the pointer still
 * looks wrong on a high-DPI display, that is a DPI-compensation bug in `dprOf`, not a
 * reason to pick a different default here.
 */

/** Must match config.rs's `overlay_thickness` default — the no-op slider position. */
export const DEFAULT_THICKNESS = 4;

/** Slider position (1-10) -> stroke weight multiplier. */
export function strokeScale(thickness: number): number {
  const t = Number.isFinite(thickness) ? thickness : DEFAULT_THICKNESS;
  // Grouped so the default divides exactly: (4-1)/3 === 1, giving 1.0 and not
  // 0.9999999999999999 as `(t - 1) * (0.4 / 3)` would.
  return t <= DEFAULT_THICKNESS
    ? 0.6 + ((t - 1) / 3) * 0.4
    : 1 + (t - DEFAULT_THICKNESS) / 6;
}
