import assert from "node:assert/strict";
import test from "node:test";

import {
  assertReleaseDoesNotRegress,
  compareReleaseVersions,
} from "./check-release-order.mjs";

test("compares semantic release versions numerically", () => {
  assert.equal(compareReleaseVersions("0.9.9", "0.10.0"), -1);
  assert.equal(compareReleaseVersions("2.0.0", "1.99.99"), 1);
  assert.equal(compareReleaseVersions("1.2.3", "1.2.3"), 0);
});

test("permits a new or idempotently repeated release", () => {
  assert.doesNotThrow(() => assertReleaseDoesNotRegress("0.4.1", "0.4.2"));
  assert.doesNotThrow(() => assertReleaseDoesNotRegress("0.4.2", "0.4.2"));
});

test("rejects moving the public latest pointer backwards", () => {
  assert.throws(
    () => assertReleaseDoesNotRegress("1.2.0", "1.1.9"),
    /older release/,
  );
});
