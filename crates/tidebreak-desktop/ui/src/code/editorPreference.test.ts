// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";

import {
  currentEditorPreference,
  openInEditorLabel,
  readStoredEditorPreference,
  resetEditorPreferenceStore,
  setEditorPreference,
} from "./editorPreference";

const STORAGE_KEY = "tidebreak.externalEditor";

beforeEach(() => {
  localStorage.clear();
  resetEditorPreferenceStore();
});

describe("external editor preference", () => {
  it("stores the choice under the shared key and reads it back", () => {
    setEditorPreference({
      editor: "custom",
      customProgram: "  /opt/homebrew/bin/nvim  ",
    });

    expect(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null")).toEqual({
      editor: "custom",
      customProgram: "/opt/homebrew/bin/nvim",
    });
    resetEditorPreferenceStore();
    expect(readStoredEditorPreference()).toEqual({
      editor: "custom",
      customProgram: "/opt/homebrew/bin/nvim",
    });
  });

  it("falls back to the default rather than throwing on a bad value", () => {
    const fallback = { editor: "vscode", customProgram: "" };
    for (const stored of [
      "not json",
      "null",
      "[]",
      '{"editor":"emacs"}',
      '{"editor":"zed","customProgram":7}',
    ]) {
      localStorage.setItem(STORAGE_KEY, stored);
      expect(readStoredEditorPreference()).toEqual(
        stored === '{"editor":"zed","customProgram":7}'
          ? { editor: "zed", customProgram: "" }
          : fallback,
      );
    }
  });

  it("names the editor in the action label, except a custom command", () => {
    expect(openInEditorLabel("jetbrains")).toBe("Open in JetBrains IDE");
    expect(openInEditorLabel("custom")).toBe("Open in editor");
  });

  it("serves the live choice, not the one storage still holds", () => {
    setEditorPreference({ editor: "zed", customProgram: "" });
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ editor: "vscode", customProgram: "" }),
    );

    expect(currentEditorPreference().editor).toBe("zed");
    expect(openInEditorLabel()).toBe("Open in Zed");
  });
});
