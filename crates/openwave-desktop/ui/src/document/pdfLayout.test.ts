import { describe, expect, it } from "vitest";

import { stabilizeMeasuredWidth } from "./pdfLayout";

describe("stabilizeMeasuredWidth", () => {
  it("rounds the first measure and never returns zero", () => {
    expect(stabilizeMeasuredWidth(null, 412.6)).toBe(413);
    expect(stabilizeMeasuredWidth(null, 0.2)).toBe(1);
  });

  it("ignores single-pixel wobble that would re-rasterise the page", () => {
    expect(stabilizeMeasuredWidth(400, 400.4)).toBe(400);
    expect(stabilizeMeasuredWidth(400, 401)).toBe(400);
    expect(stabilizeMeasuredWidth(400, 399)).toBe(400);
  });

  it("accepts a real resize past the threshold", () => {
    expect(stabilizeMeasuredWidth(400, 420)).toBe(420);
    expect(stabilizeMeasuredWidth(400, 398)).toBe(398);
  });
});
