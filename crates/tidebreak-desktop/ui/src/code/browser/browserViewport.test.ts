// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  clampCustomWidth,
  DEFAULT_CUSTOM_WIDTH,
  DEFAULT_VIEWPORT,
  MAX_CUSTOM_WIDTH,
  MIN_CUSTOM_WIDTH,
  parseViewport,
  readStoredViewport,
  restoreOrDefaultViewport,
  viewportLabel,
  viewportTargetWidth,
  VIEWPORT_PRESET_WIDTHS,
  writeStoredViewport,
  type BrowserViewport,
} from "./browserViewport";

describe("browserViewport", () => {
  describe("clampCustomWidth", () => {
    it("rounds and clamps into the valid range", () => {
      expect(clampCustomWidth(500)).toBe(500);
      expect(clampCustomWidth(100)).toBe(MIN_CUSTOM_WIDTH);
      expect(clampCustomWidth(99999)).toBe(MAX_CUSTOM_WIDTH);
      expect(clampCustomWidth(320.7)).toBe(321);
      expect(clampCustomWidth(NaN)).toBe(DEFAULT_CUSTOM_WIDTH);
      expect(clampCustomWidth(Infinity)).toBe(DEFAULT_CUSTOM_WIDTH);
    });
  });

  describe("viewportTargetWidth", () => {
    it("returns null for Fit and pixel widths for fixed presets", () => {
      expect(
        viewportTargetWidth({ preset: "fit", customWidth: 800 }),
      ).toBeNull();
      expect(viewportTargetWidth({ preset: "desktop", customWidth: 800 })).toBe(
        VIEWPORT_PRESET_WIDTHS.desktop,
      );
      expect(viewportTargetWidth({ preset: "tablet", customWidth: 800 })).toBe(
        VIEWPORT_PRESET_WIDTHS.tablet,
      );
      expect(viewportTargetWidth({ preset: "mobile", customWidth: 800 })).toBe(
        VIEWPORT_PRESET_WIDTHS.mobile,
      );
      expect(viewportTargetWidth({ preset: "custom", customWidth: 500 })).toBe(
        500,
      );
      expect(viewportTargetWidth({ preset: "custom", customWidth: 10 })).toBe(
        MIN_CUSTOM_WIDTH,
      );
    });
  });

  describe("viewportLabel", () => {
    it("produces compact toolbar labels", () => {
      expect(viewportLabel({ preset: "fit", customWidth: 800 })).toBe("Fit");
      expect(viewportLabel({ preset: "desktop", customWidth: 800 })).toBe(
        "Desktop 1440",
      );
      expect(viewportLabel({ preset: "custom", customWidth: 500 })).toBe(
        "Custom 500",
      );
    });
  });

  describe("persistence", () => {
    beforeEach(() => window.localStorage.clear());

    it("round-trips a viewport preference", () => {
      const viewport: BrowserViewport = {
        preset: "custom",
        customWidth: 480,
      };
      writeStoredViewport(viewport);
      expect(readStoredViewport()).toEqual(viewport);
    });

    it("clamps custom width on write", () => {
      writeStoredViewport({ preset: "custom", customWidth: 1 });
      expect(readStoredViewport()?.customWidth).toBe(MIN_CUSTOM_WIDTH);
    });

    it("returns the default when nothing is stored", () => {
      expect(restoreOrDefaultViewport()).toEqual(DEFAULT_VIEWPORT);
    });

    it("restores a stored preference", () => {
      writeStoredViewport({ preset: "tablet", customWidth: 800 });
      expect(restoreOrDefaultViewport()).toEqual({
        preset: "tablet",
        customWidth: 800,
      });
    });

    it("ignores malformed stored values", () => {
      window.localStorage.setItem(
        "tidebreak.code-browser-viewport.v1",
        JSON.stringify({ preset: "unknown", customWidth: 800 }),
      );
      expect(readStoredViewport()).toBeNull();
      expect(restoreOrDefaultViewport()).toEqual(DEFAULT_VIEWPORT);
    });

    it("never throws when storage is unavailable", () => {
      const broken = {
        getItem: vi.fn(() => {
          throw new Error("denied");
        }),
        setItem: vi.fn(() => {
          throw new Error("denied");
        }),
      };
      expect(readStoredViewport(broken)).toBeNull();
      expect(() => writeStoredViewport(DEFAULT_VIEWPORT, broken)).not.toThrow();
    });
  });

  describe("parseViewport", () => {
    it("accepts valid presets and clamps custom width", () => {
      expect(parseViewport({ preset: "fit", customWidth: 800 })).toEqual({
        preset: "fit",
        customWidth: 800,
      });
      expect(parseViewport({ preset: "mobile", customWidth: 50 })).toEqual({
        preset: "mobile",
        customWidth: MIN_CUSTOM_WIDTH,
      });
      expect(parseViewport({ preset: "custom" })).toEqual({
        preset: "custom",
        customWidth: DEFAULT_CUSTOM_WIDTH,
      });
    });

    it("rejects invalid preset values", () => {
      expect(parseViewport(null)).toBeNull();
      expect(parseViewport({ preset: "ultrawide" })).toBeNull();
      expect(parseViewport("fit")).toBeNull();
    });
  });
});
