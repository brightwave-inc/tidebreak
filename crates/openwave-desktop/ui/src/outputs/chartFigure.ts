/**
 * A chart output's file is a Plotly figure: `{ data, layout }`, the same shape
 * `Plotly.newPlot` takes. Nothing validates it on the way in — the model wrote
 * it — so the viewer decides here whether the bytes are a figure at all, and
 * falls back to the source view when they are not.
 */

/**
 * A figure that passed the shape check. Fields stay loosely typed: what the
 * check proves is that `data` is a list of trace objects, not that any trace
 * is one Plotly can draw. Plotly itself is the judge of that, and the viewer
 * catches what it throws.
 */
export type ChartFigure = {
  data: Record<string, unknown>[];
  layout: Record<string, unknown>;
  config?: Record<string, unknown>;
  frames?: unknown[];
};

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The figure in `content`, or null if it is not one. Never throws. */
export function parseChartFigure(content: string): ChartFigure | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(content);
  } catch {
    return null;
  }

  if (!isPlainObject(parsed)) return null;

  const { data, layout, config, frames } = parsed;
  if (!Array.isArray(data) || data.length === 0) return null;
  if (!data.every(isPlainObject)) return null;

  return {
    data,
    layout: isPlainObject(layout) ? layout : {},
    ...(isPlainObject(config) ? { config } : {}),
    ...(Array.isArray(frames) ? { frames } : {}),
  };
}
