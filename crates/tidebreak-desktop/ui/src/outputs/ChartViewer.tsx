/**
 * Chart outputs drawn as interactive Plotly figures.
 *
 * Loaded through `React.lazy` from `OutputContent` — the plotly bundle is
 * large and most sessions never open a chart, so it is fetched on first use
 * the way the PDF and spreadsheet engines are.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import Plotly from "plotly.js-dist-min";
import type { Config, Data, Layout } from "plotly.js";

import { ErrorBoundary } from "@/ErrorBoundary";
import type { DeliverablePreview } from "@/deliverables";
import { useTheme } from "@/theme";
import { CodeViewer } from "./CodeViewer";
import { parseChartFigure, type ChartFigure } from "./chartFigure";

const CHART_MEDIA_TYPE = "application/vnd.tidebreak.chart+json";
const DEFAULT_HEIGHT = 400;
const TRANSPARENT = "rgba(0,0,0,0)";

type Palette = {
  foreground: string;
  mutedForeground: string;
  border: string;
  fontFamily: string;
};

/**
 * The figure's colours come from the app's own tokens rather than Plotly's
 * stock palette, so a chart reads correctly in both themes. They have to be
 * resolved values — Plotly writes them into SVG attributes, where a
 * `var(--…)` reference means nothing — which is why this re-reads on a theme
 * change instead of handing over the variable names.
 */
function readPalette(): Palette {
  const style = getComputedStyle(document.documentElement);
  const token = (name: string, fallback: string) =>
    style.getPropertyValue(name).trim() || fallback;
  return {
    foreground: token("--foreground", "#18181b"),
    mutedForeground: token("--muted-foreground", "#71717a"),
    border: token("--border", "#e4e4e7"),
    fontFamily: token("--sans", "system-ui, sans-serif"),
  };
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const AXIS_KEY = /^[xy]axis\d*$/;

/**
 * The figure's layout with the app's styling filled in underneath it.
 *
 * Every default here is written before the figure's own value, so a figure
 * that asked for a background, a font or an axis colour keeps it — a chart is
 * the model's artifact, and the theme is only what it left unsaid.
 */
function themedLayout(
  layout: Record<string, unknown>,
  palette: Palette,
): Partial<Layout> {
  const axisDefaults = {
    gridcolor: palette.border,
    zerolinecolor: palette.border,
    linecolor: palette.border,
    tickfont: { color: palette.mutedForeground },
  };

  const axes: Record<string, unknown> = {};
  const axisKeys = new Set(["xaxis", "yaxis"]);
  for (const key of Object.keys(layout)) {
    if (AXIS_KEY.test(key)) axisKeys.add(key);
  }
  for (const key of axisKeys) {
    const figureAxis = isPlainObject(layout[key]) ? layout[key] : {};
    axes[key] = {
      ...axisDefaults,
      ...figureAxis,
      tickfont: {
        ...axisDefaults.tickfont,
        ...(isPlainObject(figureAxis.tickfont) ? figureAxis.tickfont : {}),
      },
    };
  }

  return {
    paper_bgcolor: TRANSPARENT,
    plot_bgcolor: TRANSPARENT,
    ...layout,
    ...axes,
    font: {
      family: palette.fontFamily,
      color: palette.foreground,
      size: 12,
      ...(isPlainObject(layout.font) ? layout.font : {}),
    },
  } as Partial<Layout>;
}

/** The export filename a chart's toolbar offers, without the type suffix. */
export function chartExportName(filename: string): string {
  return filename.replace(/\.chart\.json$/i, "") || "chart";
}

function chartConfig(
  figureConfig: Record<string, unknown> | undefined,
  filename: string,
): Partial<Config> {
  return {
    displaylogo: false,
    displayModeBar: "hover",
    // The stock modebar is a wall of tools most of which do not apply to a
    // static report figure; this is the set that does.
    modeBarButtons: [
      ["toImage", "zoomIn2d", "zoomOut2d", "autoScale2d", "resetScale2d"],
    ],
    ...figureConfig,
    responsive: true,
    toImageButtonOptions: {
      format: "png",
      scale: 2,
      filename: chartExportName(filename),
    },
  } as Partial<Config>;
}

function PlotlyFigure({
  data,
  layout,
  config,
  height,
}: {
  data: Record<string, unknown>[];
  layout: Partial<Layout>;
  config: Partial<Config>;
  height: number;
}) {
  const container = useRef<HTMLDivElement>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    let disposed = false;
    setFailed(false);

    // Plotly rejects a malformed trace by throwing, and a throw inside an
    // effect escapes every error boundary above it — so the failure is caught
    // here and turned into state the fallback can render.
    const draw = async () => {
      try {
        await Plotly.react(element, data as unknown as Data[], layout, config);
      } catch (error) {
        console.error("chart failed to render", error);
        if (!disposed) setFailed(true);
      }
    };
    void draw();

    // `responsive` only follows the window; the outputs panel resizes on its
    // own, so the container is watched directly.
    const observer =
      typeof ResizeObserver === "function"
        ? new ResizeObserver(() => {
            try {
              Plotly.Plots.resize(element);
            } catch {
              // A resize before the first draw completes is not interesting.
            }
          })
        : null;
    observer?.observe(element);

    return () => {
      disposed = true;
      observer?.disconnect();
      Plotly.purge(element);
    };
  }, [data, layout, config]);

  if (failed) return <FigureFallback />;

  return <div ref={container} className="w-full" style={{ height }} />;
}

function FigureFallback() {
  return (
    <p className="text-sm text-muted-foreground" role="status">
      This chart could not be drawn. The source is shown below.
    </p>
  );
}

export default function ChartViewer({
  preview,
}: {
  preview: DeliverablePreview;
}) {
  const { resolved } = useTheme();
  // The palette is read from the live stylesheet, so it is the resolved theme
  // — not any value React holds — that says when it has changed.
  const palette = useMemo(() => readPalette(), [resolved]);

  // A truncated preview is not the file, and half a figure is not a figure:
  // parsing it would only produce a misleading chart.
  const figure: ChartFigure | null = useMemo(
    () => (preview.truncated ? null : parseChartFigure(preview.content)),
    [preview.truncated, preview.content],
  );

  const layout = useMemo(
    () => (figure ? themedLayout(figure.layout, palette) : null),
    [figure, palette],
  );
  const config = useMemo(
    () => (figure ? chartConfig(figure.config, preview.filename) : null),
    [figure, preview.filename],
  );

  const source = (
    <CodeViewer content={preview.content} mediaType={CHART_MEDIA_TYPE} />
  );

  if (preview.truncated) {
    return (
      <div className="space-y-4">
        <p className="text-sm text-muted-foreground" role="status">
          This chart file is too large to preview as a chart. Saving writes the
          complete file.
        </p>
        {source}
      </div>
    );
  }

  if (!figure || !layout || !config) {
    return (
      <div className="space-y-4">
        <p className="text-sm text-muted-foreground" role="status">
          This chart file is not a valid figure.
        </p>
        {source}
      </div>
    );
  }

  const height =
    typeof figure.layout.height === "number" && figure.layout.height > 0
      ? figure.layout.height
      : DEFAULT_HEIGHT;

  return (
    <ErrorBoundary resetKey={preview.revisionId} fallback={<FigureFallback />}>
      <PlotlyFigure
        data={figure.data}
        layout={layout}
        config={config}
        height={height}
      />
    </ErrorBoundary>
  );
}
