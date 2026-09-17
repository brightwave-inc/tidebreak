import { describe, expect, it } from "vitest";
import type { CodeSessionDigest } from "../api/types";
import { nestSessionDigests, sessionTreeStatusLabel } from "./sessionTree";

const working = {
  state: { type: "working" as const },
  source: "lifecycle" as const,
};

function digest(session: string, parent?: string): CodeSessionDigest {
  return {
    workspace: null,
    session,
    kind: "interactive",
    lifecycle: "idle",
    attention: working,
    title: session,
    turn_count: 1,
    ...(parent ? { parent_session: parent } : {}),
  };
}

function flatten(
  roots: CodeSessionDigest[],
  childrenOf: Map<string, CodeSessionDigest[]>,
): string[] {
  return roots.flatMap((row) => [
    row.session,
    ...flatten(childrenOf.get(row.session) ?? [], childrenOf),
  ]);
}

describe("nestSessionDigests", () => {
  it("nests children under the parent and drops duplicate roots", () => {
    const parent = digest("parent");
    const child = digest("child", "parent");
    const { roots, childrenOf } = nestSessionDigests([parent, child]);
    expect(roots.map((row) => row.session)).toEqual(["parent"]);
    expect(childrenOf.get("parent")?.map((row) => row.session)).toEqual([
      "child",
    ]);
  });

  it("promotes children beyond the depth bound so no session disappears", () => {
    const chain = [
      digest("a"),
      digest("b", "a"),
      digest("c", "b"),
      digest("d", "c"),
      digest("e", "d"),
    ];
    const { roots, childrenOf } = nestSessionDigests(chain, 3);
    expect(roots.map((row) => row.session)).toEqual(["a", "d"]);
    expect(childrenOf.get("a")?.map((row) => row.session)).toEqual(["b"]);
    expect(childrenOf.get("d")?.map((row) => row.session)).toEqual(["e"]);
    expect(flatten(roots, childrenOf).sort()).toEqual([
      "a",
      "b",
      "c",
      "d",
      "e",
    ]);
  });
  it("breaks cycles and preserves every session once", () => {
    const input = [
      digest("a", "c"),
      digest("b", "a"),
      digest("c", "b"),
      digest("self", "self"),
    ];
    const { roots, childrenOf } = nestSessionDigests(input);
    expect(flatten(roots, childrenOf).sort()).toEqual(["a", "b", "c", "self"]);
  });

  it("keeps sibling overflow visible and handles children listed before parents", () => {
    const input = [
      digest("first", "parent"),
      digest("second", "parent"),
      digest("third", "parent"),
      digest("parent"),
    ];
    const { roots, childrenOf } = nestSessionDigests(input, 3, 1);
    expect(roots.map((row) => row.session)).toEqual([
      "parent",
      "second",
      "third",
    ]);
    expect(flatten(roots, childrenOf).sort()).toEqual([
      "first",
      "parent",
      "second",
      "third",
    ]);
  });

  it("keeps all rows as roots when nesting is disabled", () => {
    const input = [digest("a"), digest("b", "a")];
    const { roots, childrenOf } = nestSessionDigests(input, 1);
    expect(roots.map((row) => row.session)).toEqual(["a", "b"]);
    expect(childrenOf.size).toBe(0);
  });
});

describe("sessionTreeStatusLabel", () => {
  it("never names a paused child as fenced", () => {
    expect(
      sessionTreeStatusLabel({
        id: "child",
        status: "fenced",
        attention: true,
        fenced: true,
      }),
    ).toBe("Needs attention");
    expect(
      sessionTreeStatusLabel({
        id: "child",
        status: "fenced",
        attention: false,
        fenced: true,
      }),
    ).toBe("Paused");
  });
});
