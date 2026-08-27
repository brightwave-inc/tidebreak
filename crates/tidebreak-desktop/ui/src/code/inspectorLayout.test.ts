import { describe, expect, it } from "vitest";

import {
  DEFAULT_INSPECTOR_LAYOUT,
  fitsInspectorSplit,
  MIN_INSPECTOR_PANE_WIDTH_PX,
  MIN_WORKSPACE_SIZE,
  MIN_WORKSPACE_WIDTH_PX,
  usableInspectorLayout,
} from "./inspectorLayout";

describe("usableInspectorLayout", () => {
  it("keeps a bounded workspace-first split", () => {
    expect(usableInspectorLayout(DEFAULT_INSPECTOR_LAYOUT)).toEqual({
      workspace: 70,
      inspector: 30,
    });
  });

  it.each([
    undefined,
    [70, 30],
    { workspace: 0, inspector: 100 },
    { workspace: 40, inspector: 60 },
    { workspace: 70, inspector: 30, stale: 0 },
    { workspace: 70, inspector: Number.NaN },
    { workspace: 70, inspector: 20 },
    { workspace: 65, inspector: 30 },
  ])("rejects a layout that can hide or corrupt the workspace", (layout) => {
    expect(usableInspectorLayout(layout)).toBeUndefined();
  });
});

describe("fitsInspectorSplit", () => {
  it("keeps the split only where the workspace still clears its floor", () => {
    const roomy = MIN_INSPECTOR_PANE_WIDTH_PX;
    expect(fitsInspectorSplit(roomy)).toBe(true);
    expect(fitsInspectorSplit(roomy - 1)).toBe(false);
    expect((roomy * MIN_WORKSPACE_SIZE) / 100).toBeGreaterThanOrEqual(
      MIN_WORKSPACE_WIDTH_PX,
    );
  });

  it("treats an unmeasured pane as roomy", () => {
    // Deciding on the zero a pane reports before layout runs would flash the
    // inspector away on every wide-window mount.
    expect(fitsInspectorSplit(null)).toBe(true);
  });
});
