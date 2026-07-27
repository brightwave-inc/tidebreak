import { describe, expect, it } from "vitest";

import {
  encodePanelSegment,
  isValidLayout,
  layoutFromSearch,
  parsePanelSegment,
  searchFromLayout,
} from "./panelUrl";

describe("parsePanelSegment", () => {
  it("reads the bare panel names", () => {
    expect(parsePanelSegment("chat")).toEqual({ type: "chat" });
    expect(parsePanelSegment("sources")).toEqual({ type: "sources" });
    expect(parsePanelSegment("outputs")).toEqual({ type: "outputs" });
    expect(parsePanelSegment("folders")).toEqual({ type: "folders" });
  });

  it("reads a panel pointed at one of its items", () => {
    expect(parsePanelSegment("sources.0e5b1c3a")).toEqual({
      type: "sources",
      documentId: "0e5b1c3a",
    });
  });

  it("keeps the dots inside an output filename", () => {
    // Splitting on every separator would read this as an output named
    // "report" with a stray "md" hanging off it.
    expect(parsePanelSegment("outputs.quarterly report.md")).toEqual({
      type: "outputs",
      filename: "quarterly report.md",
    });
  });

  it("rejects an identifier on a panel that cannot take one", () => {
    expect(parsePanelSegment("chat.abc")).toBeNull();
    expect(parsePanelSegment("folders.abc")).toBeNull();
  });

  it("rejects a name it does not know", () => {
    expect(parsePanelSegment("reports")).toBeNull();
    expect(parsePanelSegment("")).toBeNull();
  });

  it("round-trips everything it can encode", () => {
    for (const panel of [
      { type: "chat" },
      { type: "sources" },
      { type: "sources", documentId: "doc-1" },
      { type: "outputs" },
      { type: "outputs", filename: "notes.md" },
      { type: "folders" },
    ] as const) {
      expect(parsePanelSegment(encodePanelSegment(panel))).toEqual(panel);
    }
  });
});

describe("layoutFromSearch", () => {
  it("treats no search params as the conversation alone", () => {
    expect(layoutFromSearch({})).toEqual({ mode: "single", panel: { type: "chat" } });
  });

  it("fills the unnamed side with the conversation", () => {
    expect(layoutFromSearch({ left: "sources" })).toEqual({
      mode: "split",
      left: { type: "sources" },
      right: { type: "chat" },
      fullscreen: undefined,
    });
  });

  it("carries the fullscreen side through", () => {
    const layout = layoutFromSearch({ left: "sources", right: "chat", fullscreen: "left" });
    expect(layout).toMatchObject({ mode: "split", fullscreen: "left" });
  });

  it("ignores a fullscreen value that names no side", () => {
    expect(layoutFromSearch({ left: "sources", fullscreen: "middle" })).toMatchObject({
      fullscreen: undefined,
    });
  });

  it("falls back to the conversation rather than failing on a bad URL", () => {
    // A hand-edited or stale link should land somewhere real.
    const single = { mode: "single", panel: { type: "chat" } };
    expect(layoutFromSearch({ left: "nonsense" })).toEqual(single);
    expect(layoutFromSearch({ left: "sources", right: "sources.doc-1" })).toEqual(single);
    expect(layoutFromSearch({ left: "chat", right: "chat" })).toEqual(single);
  });
});

describe("searchFromLayout", () => {
  it("clears every param for the conversation alone", () => {
    expect(searchFromLayout({ mode: "single", panel: { type: "chat" } })).toEqual({
      left: undefined,
      right: undefined,
      fullscreen: undefined,
    });
  });

  it("round-trips a split layout", () => {
    const layout = {
      mode: "split",
      left: { type: "sources", documentId: "doc-1" },
      right: { type: "chat" },
      fullscreen: "left",
    } as const;
    expect(layoutFromSearch(searchFromLayout(layout))).toEqual(layout);
  });
});

describe("isValidLayout", () => {
  it("refuses the same kind of panel on both sides", () => {
    expect(isValidLayout({ type: "sources" }, { type: "sources", documentId: "d" })).toBe(false);
    expect(isValidLayout({ type: "sources" }, { type: "chat" })).toBe(true);
  });
});
