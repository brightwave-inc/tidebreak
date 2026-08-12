/**
 * Drop sub-pixel ResizeObserver noise so a settled panel does not keep
 * re-rasterising the page. Rounding alone is not enough: classic scrollbars
 * and overlay scrollbar chrome can still wobble the content box by a pixel.
 */
export function stabilizeMeasuredWidth(
  previous: number | null,
  measured: number,
  threshold = 1,
): number {
  const rounded = Math.round(measured);
  if (previous == null) return Math.max(rounded, 1);
  if (Math.abs(rounded - previous) <= threshold) return previous;
  return Math.max(rounded, 1);
}
