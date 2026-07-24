import { describe, expect, it } from "vitest";
import {
  CHAT_SURFACE,
  isSurfaceKind,
  normalizeSurface,
  sameSurface,
} from "./Surface";

describe("normalizeSurface", () => {
  it("keeps a surface with no target", () => {
    expect(normalizeSurface({ kind: "folders" })).toEqual({ kind: "folders" });
  });

  it("keeps an item and its anchor where both are accepted", () => {
    expect(
      normalizeSurface({ kind: "documents", itemId: "doc-1", anchor: "c-2" }),
    ).toEqual({ kind: "documents", itemId: "doc-1", anchor: "c-2" });
  });

  it("drops an anchor from a surface that only addresses items", () => {
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
    const surface = { kind: "documents", itemId: "doc-1", anchor: "c-2" };
    expect(normalizeSurface(JSON.parse(JSON.stringify(surface)))).toEqual(
      surface,
    );
  });

  it("does not inherit object prototype keys as surfaces", () => {
    expect(isSurfaceKind("toString")).toBe(false);
    expect(isSurfaceKind("constructor")).toBe(false);
  });
});

describe("sameSurface", () => {
  it("compares kind and target together", () => {
    expect(
      sameSurface({ kind: "documents", itemId: "a" }, { kind: "documents", itemId: "a" }),
    ).toBe(true);
    expect(
      sameSurface({ kind: "documents", itemId: "a" }, { kind: "documents", itemId: "b" }),
    ).toBe(false);
    expect(sameSurface({ kind: "documents" }, { kind: "folders" })).toBe(false);
  });
});
