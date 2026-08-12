import { useEffect, useRef } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";

import { hasNativeHost } from "./host";

const ZOOM_KEY = "tidebreak.zoom";

/**
 * The scales the interface offers.
 *
 * Discrete steps rather than a free multiplier: each press has to be a visible
 * change, and both ends have to stop somewhere the layout still works. The rail
 * and the composer have fixed minimum widths, so a scale far past 2 leaves the
 * transcript with nothing to occupy.
 */
export const ZOOM_STEPS: readonly number[] = [
  0.7, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2,
];

export const DEFAULT_ZOOM = 1;

function stepIndexNearest(level: number): number {
  let nearest = 0;
  for (let index = 1; index < ZOOM_STEPS.length; index += 1) {
    const candidate = ZOOM_STEPS[index] as number;
    const best = ZOOM_STEPS[nearest] as number;
    if (Math.abs(candidate - level) < Math.abs(best - level)) nearest = index;
  }
  return nearest;
}

/**
 * The scale one press away from where the interface sits.
 *
 * Snapping to the nearest step first means a level restored from an older set
 * of steps — or one the platform rounded — still moves by one press rather than
 * jumping somewhere unrelated.
 */
export function nextZoom(level: number, direction: "in" | "out"): number {
  const from = stepIndexNearest(level);
  const to = direction === "in" ? from + 1 : from - 1;
  const clamped = Math.min(ZOOM_STEPS.length - 1, Math.max(0, to));
  return ZOOM_STEPS[clamped] as number;
}

/** A stored level, snapped onto the steps, or the default if there is none. */
export function readStoredZoom(): number {
  try {
    const stored = window.localStorage.getItem(ZOOM_KEY);
    if (stored === null) return DEFAULT_ZOOM;
    const level = Number.parseFloat(stored);
    if (!Number.isFinite(level)) return DEFAULT_ZOOM;
    return ZOOM_STEPS[stepIndexNearest(level)] as number;
  } catch {
    return DEFAULT_ZOOM;
  }
}

function storeZoom(level: number): void {
  try {
    window.localStorage.setItem(ZOOM_KEY, String(level));
  } catch {
    // Preference persistence is best-effort.
  }
}

/**
 * Scale the interface.
 *
 * The webview's own zoom is what's wanted rather than a CSS transform: it
 * scales text, layout, and images the way the platform's own zoom does, and
 * leaves the renderer's own measurements — the composer's line height, the
 * transcript's scroll positions — in the units they were written in.
 *
 * Outside the native host there is no webview to ask, so the document carries
 * it. That path is the browser dev server, not the shipped app.
 */
async function applyZoom(level: number): Promise<void> {
  if (hasNativeHost()) {
    try {
      await getCurrentWebview().setZoom(level);
      return;
    } catch {
      // Fall through: better a scaled document than a shortcut that does
      // nothing if the host refuses the call.
    }
  }
  document.documentElement.style.zoom = String(level);
}

export type InterfaceZoom = {
  zoomIn: () => void;
  zoomOut: () => void;
  resetZoom: () => void;
};

/**
 * Interface scale, restored on launch and persisted as it changes.
 *
 * The level is held in a ref rather than in state because nothing renders it —
 * the webview holds the truth, and a re-render on every press would be a
 * re-render for nothing.
 */
export function useInterfaceZoom(): InterfaceZoom {
  const levelRef = useRef(readStoredZoom());

  useEffect(() => {
    if (levelRef.current !== DEFAULT_ZOOM) void applyZoom(levelRef.current);
  }, []);

  function moveTo(level: number) {
    if (level === levelRef.current) return;
    levelRef.current = level;
    storeZoom(level);
    void applyZoom(level);
  }

  return {
    zoomIn: () => moveTo(nextZoom(levelRef.current, "in")),
    zoomOut: () => moveTo(nextZoom(levelRef.current, "out")),
    resetZoom: () => moveTo(DEFAULT_ZOOM),
  };
}
