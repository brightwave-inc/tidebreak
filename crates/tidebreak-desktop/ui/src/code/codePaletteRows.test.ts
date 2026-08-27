import { describe, expect, it, vi } from "vitest";

import { codeWorkspace } from "../stories/fixtures";
import {
  codeNavigationPaletteRows,
  workspaceActionPaletteRows,
} from "./codePaletteRows";
import { setEditorPreference } from "./editorPreference";

describe("codeNavigationPaletteRows", () => {
  it("puts analytics before delivery and navigates to its route", () => {
    const navigate = vi.fn();
    const rows = codeNavigationPaletteRows({
      navigate,
      onNewWorkspace: vi.fn(),
      onQuickOpen: vi.fn(),
    });

    const analytics = rows.find((row) => row.id === "navigate:analytics");
    const delivery = rows.findIndex(
      (row) => row.id === "navigate:pull-requests",
    );

    expect(analytics).toBeDefined();
    expect(rows.indexOf(analytics!)).toBeLessThan(delivery);
    analytics?.onSelect();
    expect(navigate).toHaveBeenCalledWith("/code/analytics");
  });

  it("does not keep a Delivery notifications destination", () => {
    const navigate = vi.fn();
    const rows = codeNavigationPaletteRows({
      navigate,
      onNewWorkspace: vi.fn(),
      onQuickOpen: vi.fn(),
    });

    expect(rows.some((row) => row.id === "navigate:notifications")).toBe(false);
    for (const row of rows) {
      navigate.mockClear();
      row.onSelect();
    }
    expect(navigate.mock.calls.flat()).not.toContain("/code/notifications");
  });
});

describe("workspaceActionPaletteRows", () => {
  const common = {
    workspace: codeWorkspace,
    hasPr: false,
    hasSession: false,
    attentionPinned: false,
    quickActions: [],
  };

  it("carries Open in editor with an icon, and runs the command it names", () => {
    setEditorPreference({ editor: "cursor", customProgram: "" });
    const onCommand = vi.fn();
    const rows = workspaceActionPaletteRows({
      ...common,
      canOpenInEditor: true,
      onCommand,
    });

    const row = rows.find((item) => item.id === "action:open-in-editor");
    expect(row?.label).toBe("Open in Cursor");
    // Every neighbouring action row wears a glyph; a bare row reads as a
    // second class of command.
    expect(row?.icon).toBeDefined();
    row?.onSelect();
    expect(onCommand).toHaveBeenCalledWith({
      id: "open-in-editor",
      label: "Open in Cursor",
    });
  });

  it("leaves the row out where no editor here can open the file", () => {
    for (const input of [
      { ...common, canOpenInEditor: false },
      {
        ...common,
        workspace: { ...codeWorkspace, status: "archived" as const },
        canOpenInEditor: true,
      },
    ]) {
      const rows = workspaceActionPaletteRows({ ...input, onCommand: vi.fn() });
      expect(rows.some((row) => row.id === "action:open-in-editor")).toBe(
        false,
      );
    }
  });
});
