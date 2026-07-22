import assert from "node:assert/strict";
import test from "node:test";

import { parseReleaseTag } from "./check-release-tag.mjs";

test("parses a strict semantic release tag", () => {
  assert.deepEqual(parseReleaseTag("v1.24.3"), {
    version: "1.24.3",
    major: "1",
    minor: "24",
    patch: "3",
  });
});

for (const tag of [
  "1.2.3",
  "v01.2.3",
  "v1.2",
  "v1.2.3-rc.1",
  "v0.0.0",
  "release-v1.2.3",
]) {
  test(`rejects ${tag}`, () => {
    assert.equal(parseReleaseTag(tag), null);
  });
}
