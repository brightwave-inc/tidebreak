import { describe, expect, it } from "vitest";

import { createByteCache } from "./useFileDownload";

function file(bytes: number) {
  return { bytes: new Uint8Array(bytes), contentType: null };
}

describe("createByteCache", () => {
  it("evicts the least recently read source, not the oldest one", () => {
    const cache = createByteCache(10);
    cache.set("first", file(4));
    cache.set("second", file(4));

    // The reader flips back to the first source, which makes the second the one
    // they have not looked at for longest even though it arrived later.
    expect(cache.get("first")?.bytes.byteLength).toBe(4);

    cache.set("third", file(4));

    expect(cache.get("second")).toBeUndefined();
    expect(cache.get("first")?.bytes.byteLength).toBe(4);
    expect(cache.get("third")?.bytes.byteLength).toBe(4);
  });

  it("refuses a source larger than the whole budget rather than emptying itself", () => {
    const cache = createByteCache(10);
    cache.set("workbook", file(6));
    cache.set("oversized", file(11));

    expect(cache.get("oversized")).toBeUndefined();
    expect(cache.get("workbook")?.bytes.byteLength).toBe(6);
  });

  it("does not charge twice for a source cached again", () => {
    const cache = createByteCache(10);
    cache.set("report", file(6));
    cache.set("report", file(6));
    cache.set("note", file(4));

    expect(cache.get("report")?.bytes.byteLength).toBe(6);
    expect(cache.get("note")?.bytes.byteLength).toBe(4);
  });
});
