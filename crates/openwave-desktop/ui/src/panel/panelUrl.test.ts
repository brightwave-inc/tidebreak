import { describe, expect, it } from "vitest";

import {
  encodePanelSegment,
  isValidLayout,
  layoutFromSearch,
  parsePanelSegment,
  searchFromLayout,
} from "./panelUrl";

describe("panel URLs", () => {
  it("reads navigation panels and addressed document viewers", () => {
    expect(parsePanelSegment("chat")).toEqual({ type: "chat" });
    expect(parsePanelSegment("outputs")).toEqual({ type: "outputs" });
    expect(parsePanelSegment("folders")).toEqual({ type: "folders" });
    expect(parsePanelSegment("apps")).toEqual({ type: "apps" });
    expect(parsePanelSegment("apps.app-1")).toEqual({ type: "apps", appId: "app-1" });
    expect(parsePanelSegment("document.doc-1")).toEqual({
      type: "document",
      documentId: "doc-1",
    });
    expect(parsePanelSegment("document.doc-1.cite-1")).toEqual({
      type: "document",
      documentId: "doc-1",
      citationId: "cite-1",
    });
  });

  it("keeps historical source detail links but retires the bare catalog", () => {
    expect(parsePanelSegment("sources")).toBeNull();
    expect(parsePanelSegment("sources.doc-1.cite-1")).toEqual({
      type: "document",
      documentId: "doc-1",
      citationId: "cite-1",
    });
  });

  it("rejects incomplete or overlong document targets", () => {
    expect(parsePanelSegment("document")).toBeNull();
    expect(parsePanelSegment("document..cite-1")).toBeNull();
    expect(parsePanelSegment("document.doc-1.")).toBeNull();
    expect(parsePanelSegment("document.doc-1.cite-1.extra")).toBeNull();
  });

  it("round-trips every current panel shape", () => {
    for (const panel of [
      { type: "chat" },
      { type: "document", documentId: "doc-1" },
      { type: "document", documentId: "doc-1", citationId: "cite-1" },
      { type: "outputs" },
      { type: "outputs", outputId: "output-1" },
      { type: "folders" },
    ] as const) {
      expect(parsePanelSegment(encodePanelSegment(panel))).toEqual(panel);
    }
  });
});

describe("layout URLs", () => {
  const single = { mode: "single", panel: { type: "chat" } } as const;

  it("uses a bare URL for the conversation alone", () => {
    expect(layoutFromSearch({})).toEqual(single);
    expect(searchFromLayout(single)).toEqual({
      left: undefined,
      right: undefined,
      fullscreen: undefined,
    });
  });

  it("fills the unnamed side with the conversation", () => {
    expect(layoutFromSearch({ right: "document.doc-1" })).toEqual({
      mode: "split",
      left: { type: "chat" },
      right: { type: "document", documentId: "doc-1" },
      fullscreen: undefined,
    });
  });

  it("falls back rather than opening a stale bare sources panel", () => {
    expect(layoutFromSearch({ left: "sources" })).toEqual(single);
    expect(layoutFromSearch({ left: "chat", right: "chat" })).toEqual(single);
  });

  it("round-trips a split document layout", () => {
    const layout = {
      mode: "split",
      left: { type: "chat" },
      right: { type: "document", documentId: "doc-1" },
      fullscreen: "right",
    } as const;
    expect(layoutFromSearch(searchFromLayout(layout))).toEqual(layout);
  });

  it("refuses two document viewers in one layout", () => {
    expect(
      isValidLayout(
        { type: "document", documentId: "a" },
        { type: "document", documentId: "b" },
      ),
    ).toBe(false);
    expect(
      isValidLayout({ type: "document", documentId: "a" }, { type: "chat" }),
    ).toBe(true);
  });
});
