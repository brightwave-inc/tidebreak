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

  it("bounds depth so a deep chain does not duplicate every ancestor", () => {
    const chain = [
      digest("a"),
      digest("b", "a"),
      digest("c", "b"),
      digest("d", "c"),
      digest("e", "d"),
    ];
    const { roots, childrenOf } = nestSessionDigests(chain, 3);
    expect(roots.map((row) => row.session)).toEqual(["a"]);
    expect(childrenOf.get("a")?.map((row) => row.session)).toEqual(["b"]);
    expect(childrenOf.get("d")).toBeUndefined();
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
