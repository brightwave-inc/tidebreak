/**
 * Responsive viewport simulation for the embedded browser.
 *
 * The native child webview is sized to match the selected viewport, not the
 * full editor pane. Fit uses the available surface. Presets constrain the
 * webview to a centered, clipped column at common device widths. A bounded
 * custom width lets developers reproduce exact breakpoints.
 *
 * Viewport preference is harmless presentation state: it is persisted to
 * localStorage and never sent to the native host as authority. The host only
 * receives the resulting pixel bounds.
 */

export type BrowserViewportPreset =
  | "fit"
  | "desktop"
  | "tablet"
  | "mobile"
  | "custom";

export type BrowserViewport = {
  preset: BrowserViewportPreset;
  /** Custom width in CSS pixels. Ignored unless preset is "custom". */
  customWidth: number;
};

/** Width in CSS pixels for each fixed preset. Fit has no intrinsic width. */
export const VIEWPORT_PRESET_WIDTHS: Record<
  Exclude<BrowserViewportPreset, "fit" | "custom">,
  number
> = {
  desktop: 1440,
  tablet: 768,
  mobile: 390,
};

export const VIEWPORT_PRESET_LABELS: Record<BrowserViewportPreset, string> = {
  fit: "Fit",
  desktop: "Desktop",
  tablet: "Tablet",
  mobile: "Mobile",
  custom: "Custom",
};

/** Short label for toolbar display, e.g. "Desktop 1440". */
export function viewportLabel(viewport: BrowserViewport): string {
  if (viewport.preset === "fit") return VIEWPORT_PRESET_LABELS.fit;
  if (viewport.preset === "custom") {
    return `${VIEWPORT_PRESET_LABELS.custom} ${viewport.customWidth}`;
  }
  return `${VIEWPORT_PRESET_LABELS[viewport.preset]} ${
    VIEWPORT_PRESET_WIDTHS[viewport.preset]
  }`;
}

export const MIN_CUSTOM_WIDTH = 240;
export const MAX_CUSTOM_WIDTH = 3840;
export const DEFAULT_CUSTOM_WIDTH = 1024;

export const DEFAULT_VIEWPORT: BrowserViewport = {
  preset: "fit",
  customWidth: DEFAULT_CUSTOM_WIDTH,
};

/** Clamp a raw custom-width input into the valid range. */
export function clampCustomWidth(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_CUSTOM_WIDTH;
  return Math.min(
    Math.max(Math.round(value), MIN_CUSTOM_WIDTH),
    MAX_CUSTOM_WIDTH,
  );
}

/**
 * Resolve a viewport into a target width in CSS pixels.
 *
 * Returns `null` for Fit, meaning "use the available surface width".
 */
export function viewportTargetWidth(
  viewport: BrowserViewport,
): number | null {
  if (viewport.preset === "fit") return null;
  if (viewport.preset === "custom") return clampCustomWidth(viewport.customWidth);
  return VIEWPORT_PRESET_WIDTHS[viewport.preset];
}

/**
 * Compute the pixel bounds for the native webview given the surface
 * dimensions and the active viewport.
 *
 * Fit fills the surface. Fixed presets and custom widths center a clipped
 * column inside the surface, never exceeding it. The native webview is
 * positioned at the column's left/top so it visually matches the simulated
 * viewport.
 */
export function browserViewportBounds(
  surface: { width: number; height: number },
  viewport: BrowserViewport,
): { x: number; width: number } {
  const target = viewportTargetWidth(viewport);
  if (target === null || surface.width <= 0) {
    return { x: 0, width: surface.width };
  }
  const width = Math.min(target, surface.width);
  const x = Math.round((surface.width - width) / 2);
  return { x, width };
}

/** True when the fixed/custom viewport is wider than the surface (overflow). */
export function viewportOverflows(
  surface: { width: number },
  viewport: BrowserViewport,
): boolean {
  const target = viewportTargetWidth(viewport);
  if (target === null) return false;
  return target > surface.width;
}

// --- Persistence (presentation preference only) ---

const PREFERENCE_KEY = "tidebreak.code-browser-viewport.v1";

export function readStoredViewport(
  storage: Pick<Storage, "getItem"> = window.localStorage,
): BrowserViewport | null {
  try {
    const raw = storage.getItem(PREFERENCE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    return parseViewport(parsed);
  } catch {
    return null;
  }
}

export function writeStoredViewport(
  viewport: BrowserViewport,
  storage: Pick<Storage, "getItem" | "setItem"> = window.localStorage,
): void {
  try {
    storage.setItem(
      PREFERENCE_KEY,
      JSON.stringify({
        preset: viewport.preset,
        customWidth: clampCustomWidth(viewport.customWidth),
      }),
    );
  } catch {
    // Presentation preference; never block the browser on storage.
  }
}

export function restoreOrDefaultViewport(
  storage: Pick<Storage, "getItem"> = window.localStorage,
): BrowserViewport {
  return readStoredViewport(storage) ?? DEFAULT_VIEWPORT;
}

export function parseViewport(value: unknown): BrowserViewport | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  const preset = record.preset;
  if (
    preset !== "fit" &&
    preset !== "desktop" &&
    preset !== "tablet" &&
    preset !== "mobile" &&
    preset !== "custom"
  ) {
    return null;
  }
  const rawWidth =
    typeof record.customWidth === "number" && Number.isFinite(record.customWidth)
      ? record.customWidth
      : DEFAULT_CUSTOM_WIDTH;
  return { preset, customWidth: clampCustomWidth(rawWidth) };
}
