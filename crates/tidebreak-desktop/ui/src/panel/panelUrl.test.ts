import { describe, expect, it } from "vitest";

import {
  encodePanelSegment,
  layoutFromSearch,
  parsePanelSegment,
  searchFromLayout,
} from "./panelUrl";

describe("panel URLs", () => {
  it("reads navigation panels and addressed document viewers", () => {
    expect(parsePanelSegment("outputs")).toEqual({ type: "outputs" });
    expect(parsePanelSegment("folders")).toEqual({ type: "folders" });
    expect(parsePanelSegment("permissions")).toEqual({ type: "permissions" });
    expect(parsePanelSegment("agents")).toEqual({ type: "agents" });
    expect(parsePanelSegment("agent.run-1")).toEqual({ type: "agent", runId: "run-1" });
    expect(parsePanelSegment("files")).toEqual({ type: "files" });
    expect(parsePanelSegment("files.turn-1")).toEqual({ type: "files", turnId: "turn-1" });
    expect(parsePanelSegment("diff.t.turn-1.f.src%2Flib.rs")).toEqual({
      type: "diff",
      turnId: "turn-1",
      file: "src/lib.rs",
    });
    expect(parsePanelSegment("agent")).toBeNull();
    expect(parsePanelSegment("document.doc-1.cite-1")).toEqual({
      type: "document",
      documentId: "doc-1",
      citationId: "cite-1",
    });
  });

  // The install-wide libraries became routes; links that still open them as
  // tabs fall back to the conversation alone.
  it("no longer reads the retired library panels", () => {
    expect(parsePanelSegment("apps")).toBeNull();
    expect(parsePanelSegment("apps.app-1")).toBeNull();
    expect(parsePanelSegment("plugins.documents")).toBeNull();
  });

  // The conversation holds its own region now; a URL still naming it as a
  // panel is describing a tab that does not exist.
  it("no longer reads the conversation as a panel", () => {
    expect(parsePanelSegment("chat")).toBeNull();
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
      { type: "document", documentId: "doc-1" },
      { type: "document", documentId: "doc-1", citationId: "cite-1" },
      { type: "outputs" },
      { type: "outputs", outputId: "output-1" },
      { type: "folders" },
      { type: "permissions" },
      { type: "agents" },
      { type: "agent", runId: "run-1" },
      { type: "files" },
      { type: "files", turnId: "turn-1" },
      { type: "diff" },
      { type: "diff", turnId: "turn-1" },
      { type: "diff", turnId: "turn-1", file: "src/lib.rs" },
    ] as const) {
      expect(parsePanelSegment(encodePanelSegment(panel))).toEqual(panel);
    }
  });
});

describe("layout URLs", () => {
  const alone = { tabs: [], activeIndex: 0, fullscreen: false };

  it("uses a bare URL for the conversation alone", () => {
    expect(layoutFromSearch({})).toEqual(alone);
    expect(searchFromLayout(alone)).toEqual({
      tabs: undefined,
      active: undefined,
      fullscreen: undefined,
      left: undefined,
      right: undefined,
    });
  });

  it("round-trips a strip of tabs with one of them showing", () => {
    const layout = {
      tabs: [
        { type: "folders" as const },
        { type: "document" as const, documentId: "doc-1", citationId: "cite-2" },
      ],
      activeIndex: 1,
      fullscreen: true,
    };

    expect(searchFromLayout(layout)).toMatchObject({
      tabs: "folders,document.doc-1.cite-2",
      active: "document.doc-1.cite-2",
      fullscreen: "1",
    });
    expect(layoutFromSearch(searchFromLayout(layout))).toEqual(layout);
  });

  it("falls back to the first tab when nothing usable is named active", () => {
    expect(layoutFromSearch({ tabs: "folders,outputs", active: "agents" }).activeIndex).toBe(0);
    expect(layoutFromSearch({ tabs: "folders,outputs" }).activeIndex).toBe(0);
  });

  it("drops segments that address nothing, and the conversation with them", () => {
    expect(layoutFromSearch({ tabs: "chat,folders,nonsense" })).toEqual({
      tabs: [{ type: "folders" }],
      activeIndex: 0,
      fullscreen: false,
    });
    expect(layoutFromSearch({ tabs: "chat" })).toEqual(alone);
  });

  it("reads one tab per panel however often the URL names it", () => {
    // Both segments address the same document, which is one tab open at one
    // of the two places in it.
    expect(layoutFromSearch({ tabs: "document.doc-1,document.doc-1.cite-2" }).tabs).toEqual([
      { type: "document", documentId: "doc-1" },
    ]);
  });

  it("restores a link written in the retired pair-of-slots grammar", () => {
    expect(layoutFromSearch({ left: "folders", right: "document.doc-1" })).toEqual({
      tabs: [{ type: "folders" }, { type: "document", documentId: "doc-1" }],
      activeIndex: 0,
      fullscreen: false,
    });
    // The conversation used to fill the slot it was not sharing.
    expect(layoutFromSearch({ left: "chat", right: "outputs.out-9" })).toEqual({
      tabs: [{ type: "outputs", outputId: "out-9" }],
      activeIndex: 0,
      fullscreen: false,
    });
    expect(layoutFromSearch({ left: "chat", right: "chat" })).toEqual(alone);
  });

  it("carries a legacy expanded panel over, but not an expanded conversation", () => {
    expect(
      layoutFromSearch({ left: "chat", right: "document.doc-1", fullscreen: "right" }),
    ).toMatchObject({ fullscreen: true });
    expect(
      layoutFromSearch({ left: "chat", right: "document.doc-1", fullscreen: "left" }),
    ).toMatchObject({ fullscreen: false });
  });

  it("clears the retired params whenever it writes a layout", () => {
    expect(searchFromLayout(layoutFromSearch({ left: "folders" }))).toMatchObject({
      tabs: "folders",
      left: undefined,
      right: undefined,
    });
  });
});
