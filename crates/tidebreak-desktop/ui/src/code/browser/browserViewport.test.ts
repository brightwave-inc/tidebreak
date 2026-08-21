// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  browserViewportBounds,
  clampCustomWidth,
  DEFAULT_CUSTOM_WIDTH,
  DEFAULT_VIEWPORT,
  MAX_CUSTOM_WIDTH,
  MIN_CUSTOM_WIDTH,
  parseViewport,
  readStoredViewport,
  restoreOrDefaultViewport,
  viewportLabel,
  viewportOverflows,
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
      expect(viewportTargetWidth({ preset: "fit", customWidth: 800 })).toBeNull();
      expect(viewportTargetWidth({ preset: "desktop", customWidth: 800 })).toBe(
        VIEWPORT_PRESET_WIDTHS.desktop,
      );
      expect(viewportTargetWidth({ preset: "tablet", customWidth: 800 })).toBe(
        VIEWPORT_PRESET_WIDTHS.tablet,
      );
      expect(viewportTargetWidth({ preset: "mobile", customWidth: 800 })).toBe(
        VIEWPORT_PRESET_WIDTHS.mobile,
      );
      expect(viewportTargetWidth({ preset: "custom", customWidth: 500 })).toBe(500);
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

  describe("browserViewportBounds", () => {
    it("fills the surface for Fit", () => {
      expect(
        browserViewportBounds({ width: 900, height: 600 }, {
          preset: "fit",
          customWidth: 800,
        }),
      ).toEqual({ x: 0, width: 900 });
    });

    it("centers a desktop preset that fits", () => {
      const bounds = browserViewportBounds(
        { width: 1600, height: 800 },
        { preset: "desktop", customWidth: 800 },
      );
      expect(bounds.width).toBe(VIEWPORT_PRESET_WIDTHS.desktop);
      expect(bounds.x).toBe(Math.round((1600 - 1440) / 2));
    });

    it("clamps a preset wider than the surface and centers at zero", () => {
      const bounds = browserViewportBounds(
        { width: 600, height: 400 },
        { preset: "desktop", customWidth: 800 },
      );
      expect(bounds.width).toBe(600);
      expect(bounds.x).toBe(0);
    });

    it("centers a custom width inside the surface", () => {
      const bounds = browserViewportBounds(
        { width: 1000, height: 600 },
        { preset: "custom", customWidth: 500 },
      );
      expect(bounds.width).toBe(500);
      expect(bounds.x).toBe(250);
    });

    it("handles a zero-width surface gracefully", () => {
      expect(
        browserViewportBounds({ width: 0, height: 400 }, {
          preset: "desktop",
          customWidth: 800,
        }),
      ).toEqual({ x: 0, width: 0 });
    });
  });

  describe("viewportOverflows", () => {
    it("is false for Fit and true when the target exceeds the surface", () => {
      expect(
        viewportOverflows({ width: 600 }, { preset: "fit", customWidth: 800 }),
      ).toBe(false);
      expect(
        viewportOverflows({ width: 600 }, { preset: "desktop", customWidth: 800 }),
      ).toBe(true);
      expect(
        viewportOverflows({ width: 1600 }, { preset: "desktop", customWidth: 800 }),
      ).toBe(false);
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
