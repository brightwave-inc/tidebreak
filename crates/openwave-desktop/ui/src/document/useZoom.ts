import { create } from "zustand";

const ZOOM_LEVELS = [
  25, 33, 50, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500,
] as const;

const MIN_ZOOM = ZOOM_LEVELS[0];
const MAX_ZOOM = ZOOM_LEVELS[ZOOM_LEVELS.length - 1];

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

type ZoomState = {
  scale: number;
  inputValue: string;

  setScale: (scale: number) => void;
  onInputChange: (value: string) => void;
  zoomIn: () => void;
  zoomOut: () => void;
  updateScale: () => void;
  cancelInput: () => void;
};

/**
 * Zoom is a single app-wide value rather than viewer state so the toolbar and
 * the panel header can drive the same number without either owning it.
 */
export const useZoom = create<ZoomState>((set) => ({
  scale: 100,
  inputValue: "100%",

  setScale(value) {
    const scale = clamp(Math.round(value), MIN_ZOOM, MAX_ZOOM);
    set({ scale, inputValue: `${scale}%` });
  },

  onInputChange(value) {
    set({ inputValue: value });
  },

  zoomIn() {
    set((s) => {
      const nextZoom = ZOOM_LEVELS.find((zoom) => zoom > s.scale);
      return nextZoom ? { scale: nextZoom, inputValue: `${nextZoom}%` } : {};
    });
  },

  zoomOut() {
    set((s) => {
      const index = ZOOM_LEVELS.findIndex((zoom) => zoom >= s.scale) - 1;
      const previousZoom = index >= 0 ? ZOOM_LEVELS[index] : undefined;
      return previousZoom
        ? { scale: previousZoom, inputValue: `${previousZoom}%` }
        : {};
    });
  },

  updateScale() {
    set((s) => {
      const inputValue = s.inputValue.trim();
      if (/^\d+(\.\d+)?%?$/.test(inputValue)) {
        const scale = clamp(
          Math.round(parseFloat(inputValue.replace("%", ""))),
          MIN_ZOOM,
          MAX_ZOOM,
        );
        return { scale, inputValue: `${scale}%` };
      }
      // Unparseable input reverts rather than resetting to a default.
      return { inputValue: `${s.scale}%` };
    });
  },

  cancelInput() {
    set((s) => ({ inputValue: `${s.scale}%` }));
  },
}));
