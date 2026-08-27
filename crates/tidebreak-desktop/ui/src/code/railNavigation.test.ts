import { describe, expect, it } from "vitest";

import { nextWorkspaceAfterLeaving, stepWorkspaceId } from "./railNavigation";

describe("stepWorkspaceId", () => {
  const rail = ["a", "b", "c"];

  it("cycles the rail in both directions", () => {
    // A rail is a ring: stopping at the last card would make the chord feel
    // broken exactly when the reader is furthest from the top.
    expect(stepWorkspaceId(rail, "a", 1)).toBe("b");
    expect(stepWorkspaceId(rail, "c", 1)).toBe("a");
    expect(stepWorkspaceId(rail, "a", -1)).toBe("c");
    expect(stepWorkspaceId(rail, "b", -1)).toBe("a");
  });

  it("enters the rail at the end it is walking towards", () => {
    // From the code home there is no current workspace, and
    // doing nothing would leave the reader with no keyboard way onto the rail.
    expect(stepWorkspaceId(rail, undefined, 1)).toBe("a");
    expect(stepWorkspaceId(rail, undefined, -1)).toBe("c");
    // A workspace the rail no longer draws — archived while it was open — is
    // the same situation: there is no position to step from.
    expect(stepWorkspaceId(rail, "gone", 1)).toBe("a");
  });

  it("has nowhere to go on an empty rail", () => {
    expect(stepWorkspaceId([], undefined, 1)).toBeNull();
    expect(stepWorkspaceId([], "a", -1)).toBeNull();
  });
});

describe("nextWorkspaceAfterLeaving", () => {
  it("opens the next live card, wrapping at the end of the rail", () => {
    expect(nextWorkspaceAfterLeaving(["a", "b", "c"], "a")).toBe("b");
    expect(nextWorkspaceAfterLeaving(["a", "b", "c"], "c")).toBe("a");
  });

  it("falls through to the code home when nothing else is live", () => {
    expect(nextWorkspaceAfterLeaving(["a"], "a")).toBeNull();
    expect(nextWorkspaceAfterLeaving([], "a")).toBeNull();
  });
});
