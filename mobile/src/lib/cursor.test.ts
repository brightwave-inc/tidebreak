import { describe, expect, it } from "vitest";
import { resumeAfter, shouldApplyDurable } from "./cursor";

describe("cursor resume", () => {
  it("advances only on durable frames", () => {
    expect(resumeAfter(3, { seq: 4 })).toBe(4);
    expect(resumeAfter(3, { seq: 2 })).toBe(3);
    expect(resumeAfter(3, { seq: 9, transient: true })).toBe(3);
  });

  it("applies transients even when seq does not move", () => {
    expect(shouldApplyDurable(5, { seq: 5, transient: true })).toBe(true);
    expect(shouldApplyDurable(5, { seq: 5 })).toBe(false);
    expect(shouldApplyDurable(5, { seq: 6 })).toBe(true);
  });
});
