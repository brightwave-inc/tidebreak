import { describe, expect, it, vi } from "vitest";

import {
  CODE_EDITOR_DRAG_TYPE,
  hasEditorTabDrag,
  readEditorTabDrag,
  writeEditorTabDrag,
} from "./editorDrag";

function transfer() {
  const data = new Map<string, string>();
  const value = {
    effectAllowed: "none",
    types: [] as string[],
    setData: vi.fn((type: string, payload: string) => {
      data.set(type, payload);
      if (!value.types.includes(type)) value.types.push(type);
    }),
    getData: vi.fn((type: string) => data.get(type) ?? ""),
  };
  return value;
}

describe("editor tab drag payload", () => {
  it("round-trips the tab through the private transfer type", () => {
    const data = transfer();

    writeEditorTabDrag(data as unknown as DataTransfer, {
      region: "primary",
      index: 2,
    });

    expect(data.effectAllowed).toBe("move");
    expect(data.setData).toHaveBeenCalledWith(
      CODE_EDITOR_DRAG_TYPE,
      JSON.stringify({ region: "primary", index: 2 }),
    );
    expect(hasEditorTabDrag(data as unknown as DataTransfer)).toBe(true);
    expect(readEditorTabDrag(data)).toEqual({ region: "primary", index: 2 });
  });

  it("accepts the plain-text compatibility payload", () => {
    expect(
      readEditorTabDrag({
        getData: (type) =>
          type === "text/plain"
            ? "tidebreak-editor-tab:secondary:4"
            : "",
      }),
    ).toEqual({ region: "secondary", index: 4 });
  });

  it("keeps the text fallback when a webview rejects custom MIME data", () => {
    const data = new Map<string, string>();
    const value = {
      effectAllowed: "none",
      types: [] as string[],
      setData: vi.fn((type: string, payload: string) => {
        if (type === CODE_EDITOR_DRAG_TYPE) throw new Error("unsupported type");
        data.set(type, payload);
        value.types.push(type);
      }),
      getData: vi.fn((type: string) => data.get(type) ?? ""),
    };

    expect(() =>
      writeEditorTabDrag(value as unknown as DataTransfer, {
        region: "primary",
        index: 3,
      }),
    ).not.toThrow();
    expect(hasEditorTabDrag(value as unknown as DataTransfer)).toBe(true);
    expect(readEditorTabDrag(value)).toEqual({ region: "primary", index: 3 });
  });

  it("rejects malformed and negative indexes", () => {
    expect(
      readEditorTabDrag({
        getData: (type) =>
          type === CODE_EDITOR_DRAG_TYPE
            ? JSON.stringify({ region: "primary", index: -1 })
            : "not-a-tab",
      }),
    ).toBeNull();
    expect(
      readEditorTabDrag({
        getData: (type) => (type === "text/plain" ? "primary:1" : ""),
      }),
    ).toBeNull();
  });
});
