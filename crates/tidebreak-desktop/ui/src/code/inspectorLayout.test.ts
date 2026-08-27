import { describe, expect, it } from "vitest";

import {
  DEFAULT_INSPECTOR_LAYOUT,
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
