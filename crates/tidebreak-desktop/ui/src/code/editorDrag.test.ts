import { describe, expect, it } from "vitest";

import type { LayoutState, PanelContent } from "@/panel/panelTypes";

import {
  dropEditorTab,
  editorStripDropId,
  editorTabDragId,
  findEditorPanel,
  offersSplitDrop,
  EDITOR_SPLIT_DROP_ID,
} from "./editorDrag";

const lib: PanelContent = { type: "file", path: "src/lib.rs" };
const main: PanelContent = { type: "file", path: "src/main.rs" };
const diff: PanelContent = { type: "diff", path: "src/main.rs" };

const shell: PanelContent = { type: "terminal", terminalId: "t1" };

/** A left group holding four tabs, one of them a shell. */
function layout(overrides: Partial<LayoutState> = {}): LayoutState {
  return {
    tabs: [lib, shell, main, diff],
    activeIndex: 0,
    fullscreen: false,
    ...overrides,
  };
}

const tabId = (region: "primary" | "secondary", panel: PanelContent) =>
  editorTabDragId(region, panel);

describe("editor tab drops", () => {
  it("reorders within a group, shells included", () => {
    const next = dropEditorTab(
      layout(),
      tabId("primary", lib),
      tabId("primary", diff),
    );

    expect(next?.tabs).toEqual([shell, main, diff, lib]);
    // And the tab that was active still is, wherever it ended up.
    expect(next?.activeIndex).toBe(3);
  });

  it("drags a shell like any other tab", () => {
    const next = dropEditorTab(
      layout(),
      tabId("primary", shell),
      tabId("primary", diff),
    );

    expect(next?.tabs).toEqual([lib, main, diff, shell]);
  });

  it("appends when the drop lands on the strip's open space", () => {
    const next = dropEditorTab(
      layout(),
      tabId("primary", lib),
      editorStripDropId("primary"),
    );

    expect(next?.tabs).toEqual([shell, main, diff, lib]);
  });

  it("creates the split from the zone, and reads back across it", () => {
    const split = dropEditorTab(
      layout(),
      tabId("primary", diff),
      EDITOR_SPLIT_DROP_ID,
    );
    expect(split?.editorSplit?.tabs).toEqual([diff]);
    expect(split?.tabs).toEqual([lib, shell, main]);

    // Dropping it on the left strip sends it home again.
    const back = dropEditorTab(
      split!,
      tabId("secondary", diff),
      editorStripDropId("primary"),
    );
    expect(back?.editorSplit?.tabs ?? []).toEqual([]);
    expect(back?.tabs).toContainEqual(diff);
  });

  it("reports every no-op drag as null", () => {
    const state = layout();
    // Released over open air, and dropped back onto itself.
    expect(dropEditorTab(state, tabId("primary", lib), null)).toBeNull();
    expect(
      dropEditorTab(state, tabId("primary", lib), tabId("primary", lib)),
    ).toBeNull();
    // Aimed at a tab that closed underneath the drag.
    expect(
      dropEditorTab(
        state,
        tabId("primary", lib),
        tabId("primary", {
          type: "file",
          path: "src/gone.rs",
        }),
      ),
    ).toBeNull();
    // Dragging something that is not one of ours, onto something that is not
    // one of our targets.
    expect(dropEditorTab(state, "sidebar-item", "trash")).toBeNull();
    // The split zone only ever takes a left-group tab.
    expect(
      dropEditorTab(state, tabId("secondary", lib), EDITOR_SPLIT_DROP_ID),
    ).toBeNull();
  });

  it("offers the split zone only while there is a split to create", () => {
    const state = layout();
    expect(offersSplitDrop(state, tabId("primary", lib))).toBe(true);
    expect(offersSplitDrop(state, tabId("secondary", lib))).toBe(false);

    const split = dropEditorTab(
      state,
      tabId("primary", diff),
      EDITOR_SPLIT_DROP_ID,
    );
    expect(offersSplitDrop(split!, tabId("primary", lib))).toBe(false);
  });

  it("finds the panel the overlay draws, and only while it exists", () => {
    const state = layout();
    expect(findEditorPanel(state, tabId("primary", main))).toEqual(main);
    expect(findEditorPanel(state, tabId("secondary", main))).toBeNull();
    expect(findEditorPanel(state, editorStripDropId("primary"))).toBeNull();
  });
});
