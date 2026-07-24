import { describe, expect, it } from "vitest";
import { CHAT_SURFACE, normalizeSurface, sameSurface } from "./Surface";

describe("normalizeSurface", () => {
  it("keeps a surface with no target", () => {
    expect(normalizeSurface({ kind: "folders" })).toEqual({ kind: "folders" });
  });

  it("keeps an item on the surface whose view resolves one", () => {
    expect(normalizeSurface({ kind: "deliverables", itemId: "report.md" })).toEqual(
      { kind: "deliverables", itemId: "report.md" },
    );
  });

  it("drops a target no view can act on, and anything left over", () => {
    // Sources takes only a conversation, so a target aimed at it opens the
    // list rather than an empty pane.
    expect(normalizeSurface({ kind: "documents", itemId: "doc-1" })).toEqual({
      kind: "documents",
    });
    // A stored descriptor from an older build may still carry an anchor.
    expect(
      normalizeSurface({
        kind: "deliverables",
        itemId: "report.md",
        anchor: "c-2",
      }),
    ).toEqual({ kind: "deliverables", itemId: "report.md" });
  });

  it("drops a target from a surface that accepts none", () => {
    expect(normalizeSurface({ kind: "chat", itemId: "doc-1" })).toEqual({
      kind: "chat",
    });
  });

  it("drops an anchor with no item to anchor to", () => {
    expect(normalizeSurface({ kind: "documents", anchor: "c-2" })).toEqual({
      kind: "documents",
    });
  });

  it("ignores blank and non-string identifiers", () => {
    expect(normalizeSurface({ kind: "documents", itemId: "   " })).toEqual({
      kind: "documents",
    });
    expect(normalizeSurface({ kind: "documents", itemId: 7 })).toEqual({
      kind: "documents",
    });
  });

  it("trims surrounding whitespace from identifiers", () => {
    expect(
      normalizeSurface({ kind: "deliverables", itemId: " notes.md " }),
    ).toEqual({ kind: "deliverables", itemId: "notes.md" });
  });

  it("falls back to the transcript for anything unrecognisable", () => {
    for (const value of [
      undefined,
      null,
      "documents",
      [],
      {},
      { kind: "audit" },
      { kind: 3 },
    ]) {
      expect(normalizeSurface(value)).toEqual(CHAT_SURFACE);
    }
  });

  it("survives a round trip through storage", () => {
    const surface = { kind: "deliverables", itemId: "notes.md" };
    expect(normalizeSurface(JSON.parse(JSON.stringify(surface)))).toEqual(
      surface,
    );
  });

  it("does not inherit object prototype keys as surfaces", () => {
    // A prototype key must not pass for a surface name, or a stored layout
    // could name one that has no view.
    for (const key of ["toString", "constructor", "__proto__"]) {
      expect(normalizeSurface({ kind: key })).toEqual(CHAT_SURFACE);
    }
  });
});

describe("sameSurface", () => {
  it("compares kind and target together", () => {
    expect(
      sameSurface(
        { kind: "deliverables", itemId: "a" },
        { kind: "deliverables", itemId: "a" },
      ),
    ).toBe(true);
    expect(
      sameSurface(
        { kind: "deliverables", itemId: "a" },
        { kind: "deliverables", itemId: "b" },
      ),
    ).toBe(false);
    expect(sameSurface({ kind: "documents" }, { kind: "folders" })).toBe(false);
  });
});
