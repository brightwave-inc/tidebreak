import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const mobileRoot = join(here, "../..");
const repoRoot = join(mobileRoot, "..");

describe("wire types", () => {
  it("matches the desktop generated file byte-for-byte", () => {
    const desktop = readFileSync(
      join(repoRoot, "crates/tidebreak-desktop/ui/src/generated/wire.ts"),
    );
    const mobile = readFileSync(join(mobileRoot, "src/generated/wire.ts"));
    expect(Buffer.compare(desktop, mobile)).toBe(0);
  });
});
