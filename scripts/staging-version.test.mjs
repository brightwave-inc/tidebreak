import assert from "node:assert/strict";
import test from "node:test";

import {
  assertStagingDoesNotRegress,
  compareStagingVersions,
  parseStagingVersion,
  stagingTag,
  stagingVersionFromRunNumber,
} from "./staging-version.mjs";

test("staging versions are 0.0.0-staging.N", () => {
  assert.deepEqual(parseStagingVersion("0.0.0-staging.12"), {
    version: "0.0.0-staging.12",
    build: "12",
  });
  assert.equal(parseStagingVersion("0.0.0"), null);
  assert.equal(parseStagingVersion("0.0.0-staging.0"), null);
  assert.equal(parseStagingVersion("v0.0.0-staging.12"), null);
  assert.equal(parseStagingVersion("0.36.0-staging.12"), null);
  assert.equal(parseStagingVersion("0.0.0-staging.01"), null);
});

test("run numbers mint a monotonic staging version and tag", () => {
  assert.equal(stagingVersionFromRunNumber(1), "0.0.0-staging.1");
  assert.equal(stagingVersionFromRunNumber("42"), "0.0.0-staging.42");
  assert.equal(stagingTag("0.0.0-staging.42"), "staging-42");
  assert.throws(() => stagingVersionFromRunNumber(0), /run number/);
  assert.throws(() => stagingVersionFromRunNumber("nope"), /run number/);
});

test("newer staging builds may replace older ones", () => {
  assert.equal(compareStagingVersions("0.0.0-staging.9", "0.0.0-staging.10"), -1);
  assert.doesNotThrow(() =>
    assertStagingDoesNotRegress("0.0.0-staging.9", "0.0.0-staging.10"),
  );
  assert.doesNotThrow(() =>
    assertStagingDoesNotRegress("0.0.0-staging.10", "0.0.0-staging.10"),
  );
  assert.throws(
    () => assertStagingDoesNotRegress("0.0.0-staging.10", "0.0.0-staging.9"),
    /older staging/,
  );
});
