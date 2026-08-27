import { describe, expect, it } from "vitest";

import {
  pointerSelectIntent,
  rangeWorkspaceSelection,
  seedOpenWorkspaceSelection,
  toggleWorkspaceSelection,
} from "./workspaceSelection";

const visible = ["a", "b", "c", "d"];

describe("pointerSelectIntent", () => {
  it("opens on a plain click, toggles on cmd/ctrl, and ranges on shift", () => {
    expect(
      pointerSelectIntent({ shiftKey: false, metaKey: false, ctrlKey: false }),
    ).toBe("open");
    expect(
      pointerSelectIntent({ shiftKey: false, metaKey: true, ctrlKey: false }),
    ).toBe("toggle");
    expect(
      pointerSelectIntent({ shiftKey: false, metaKey: false, ctrlKey: true }),
    ).toBe("toggle");
    expect(
      pointerSelectIntent({ shiftKey: true, metaKey: false, ctrlKey: false }),
    ).toBe("range");
    expect(
      pointerSelectIntent({ shiftKey: true, metaKey: true, ctrlKey: false }),
    ).toBe("range");
  });
});

describe("toggleWorkspaceSelection", () => {
  it("adds a missing id and drops one that is already selected", () => {
    expect(toggleWorkspaceSelection(["a"], "c")).toEqual(["a", "c"]);
    expect(toggleWorkspaceSelection(["a", "c"], "a")).toEqual(["c"]);
  });
});

describe("seedOpenWorkspaceSelection", () => {
  it("starts a gesture with the open workspace when selection is empty", () => {
    expect(seedOpenWorkspaceSelection([], "a", visible, "c")).toEqual({
      selected: ["a"],
      anchorId: "a",
    });
  });

  it("does not seed when you cmd-click the open workspace itself", () => {
    expect(seedOpenWorkspaceSelection([], "a", visible, "a")).toEqual({
      selected: [],
      anchorId: "a",
    });
  });

  it("leaves an existing selection alone", () => {
    expect(seedOpenWorkspaceSelection(["b"], "a", visible, "c")).toEqual({
      selected: ["b"],
      anchorId: null,
    });
  });
});

describe("rangeWorkspaceSelection", () => {
  it("selects the inclusive span from the anchor through the target", () => {
    expect(rangeWorkspaceSelection(visible, "a", "c")).toEqual(["a", "b", "c"]);
    expect(rangeWorkspaceSelection(visible, "d", "b")).toEqual(["b", "c", "d"]);
  });

  it("falls back to the target when the anchor is missing or off the rail", () => {
    expect(rangeWorkspaceSelection(visible, null, "c")).toEqual(["c"]);
    expect(rangeWorkspaceSelection(visible, "gone", "c")).toEqual(["c"]);
  });

  it("crosses a group boundary because visible order is already flattened", () => {
    expect(rangeWorkspaceSelection(visible, "b", "d")).toEqual(["b", "c", "d"]);
  });
});
