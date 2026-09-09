import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_STAGING_RELEASE_KEEP_COUNT,
  assertDeletableStagingReleaseKey,
  assertDeletableStagingReleasePrefix,
  parseStagingReleasePrefix,
  planStagingReleasePrune,
  stagingReleasePrefix,
} from "./prune-staging-releases.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const script = join(here, "prune-staging-releases.mjs");
const shell = readFileSync(join(here, "prune-staging-releases.sh"), "utf8");

function prefix(version) {
  return stagingReleasePrefix(version);
}

test("staging release prefixes are versioned directories under the staging channel", () => {
  assert.equal(
    prefix("0.0.0-staging.12"),
    "tidebreak/staging/releases/v0.0.0-staging.12/",
  );
  assert.deepEqual(parseStagingReleasePrefix(prefix("0.0.0-staging.12")), {
    version: "0.0.0-staging.12",
    build: "12",
  });
  assert.equal(
    parseStagingReleasePrefix("tidebreak/staging/releases/v0.0.0-staging.12"),
    null,
  );
  assert.equal(
    parseStagingReleasePrefix("tidebreak/staging/releases/v0.0.0-staging.1"),
    null,
  );
  assert.equal(
    parseStagingReleasePrefix("tidebreak/releases/v0.4.2/"),
    null,
  );
  assert.equal(
    parseStagingReleasePrefix("tidebreak/staging/releases/v0.4.2/"),
    null,
  );
  assert.equal(
    parseStagingReleasePrefix("tidebreak/staging/releases/v0.0.0-staging.01/"),
    null,
  );
  assert.equal(DEFAULT_STAGING_RELEASE_KEEP_COUNT, 3);
});

test("prune keeps the live staging build and the newest neighbors", () => {
  assert.deepEqual(
    planStagingReleasePrune({
      latestVersion: "0.0.0-staging.100",
      prefixes: [
        prefix("0.0.0-staging.97"),
        prefix("0.0.0-staging.98"),
        prefix("0.0.0-staging.99"),
        prefix("0.0.0-staging.100"),
      ],
    }),
    {
      latestVersion: "0.0.0-staging.100",
      keepCount: 3,
      keep: [
        prefix("0.0.0-staging.100"),
        prefix("0.0.0-staging.99"),
        prefix("0.0.0-staging.98"),
      ],
      delete: [prefix("0.0.0-staging.97")],
      skipped: [],
    },
  );

  // Live is not the newest listing entry: still retain live + two newest others.
  assert.deepEqual(
    planStagingReleasePrune({
      latestVersion: "0.0.0-staging.100",
      prefixes: [
        prefix("0.0.0-staging.100"),
        prefix("0.0.0-staging.101"),
        prefix("0.0.0-staging.102"),
        prefix("0.0.0-staging.99"),
        prefix("0.0.0-staging.98"),
      ],
    }),
    {
      latestVersion: "0.0.0-staging.100",
      keepCount: 3,
      keep: [
        prefix("0.0.0-staging.100"),
        prefix("0.0.0-staging.102"),
        prefix("0.0.0-staging.101"),
      ],
      delete: [prefix("0.0.0-staging.98"), prefix("0.0.0-staging.99")],
      skipped: [],
    },
  );

  assert.deepEqual(
    planStagingReleasePrune({
      latestVersion: "0.0.0-staging.5",
      keep: 1,
      prefixes: [
        prefix("0.0.0-staging.5"),
        prefix("0.0.0-staging.4"),
        prefix("0.0.0-staging.6"),
      ],
    }),
    {
      latestVersion: "0.0.0-staging.5",
      keepCount: 1,
      keep: [prefix("0.0.0-staging.5")],
      delete: [prefix("0.0.0-staging.4"), prefix("0.0.0-staging.6")],
      skipped: [],
    },
  );
});

test("prune never deletes the live prefix even when it is absent from the listing", () => {
  const plan = planStagingReleasePrune({
    latestVersion: "0.0.0-staging.100",
    prefixes: [
      prefix("0.0.0-staging.99"),
      prefix("0.0.0-staging.98"),
      prefix("0.0.0-staging.97"),
      prefix("0.0.0-staging.96"),
    ],
  });
  assert.deepEqual(plan.keep, [
    prefix("0.0.0-staging.99"),
    prefix("0.0.0-staging.98"),
    prefix("0.0.0-staging.97"),
  ]);
  assert.deepEqual(plan.delete, [prefix("0.0.0-staging.96")]);
  assert.ok(!plan.delete.includes(prefix("0.0.0-staging.100")));
});

test("prune skips unknown prefixes instead of deleting them", () => {
  const plan = planStagingReleasePrune({
    latestVersion: "0.0.0-staging.2",
    prefixes: [
      prefix("0.0.0-staging.2"),
      prefix("0.0.0-staging.1"),
      "tidebreak/staging/releases/v0.4.2/",
      "tidebreak/staging/releases/v0.0.0-staging.3",
      "tidebreak/staging/latest.json",
      "",
      prefix("0.0.0-staging.2"),
    ],
  });
  assert.deepEqual(plan.keep, [
    prefix("0.0.0-staging.2"),
    prefix("0.0.0-staging.1"),
  ]);
  assert.deepEqual(plan.delete, []);
  assert.deepEqual(plan.skipped, [
    "tidebreak/staging/latest.json",
    "tidebreak/staging/releases/v0.0.0-staging.3",
    "tidebreak/staging/releases/v0.4.2/",
  ]);
});

test("prune refuses an invalid live version or keep count", () => {
  assert.throws(
    () =>
      planStagingReleasePrune({
        latestVersion: "0.4.2",
        prefixes: [],
      }),
    /invalid staging version/,
  );
  assert.throws(
    () =>
      planStagingReleasePrune({
        latestVersion: "0.0.0-staging.1",
        prefixes: [],
        keep: 0,
      }),
    /keep count/,
  );
});

test("delete assertions refuse production objects, the live prefix, and feed files", () => {
  const live = "0.0.0-staging.12";
  assert.doesNotThrow(() =>
    assertDeletableStagingReleasePrefix(prefix("0.0.0-staging.11"), live),
  );
  assert.doesNotThrow(() =>
    assertDeletableStagingReleaseKey(
      `${prefix("0.0.0-staging.11")}macos/universal/Tidebreak_0.0.0-staging.11_universal.dmg`,
      live,
    ),
  );

  assert.throws(
    () => assertDeletableStagingReleasePrefix(prefix(live), live),
    /live staging prefix/,
  );
  assert.throws(
    () =>
      assertDeletableStagingReleasePrefix(
        "tidebreak/staging/releases/v0.0.0-staging.11",
        live,
      ),
    /not a deletable staging release prefix/,
  );
  assert.throws(
    () =>
      assertDeletableStagingReleasePrefix(
        "tidebreak/staging/releases/v0.0.0-staging.1",
        live,
      ),
    /not a deletable staging release prefix/,
  );
  assert.throws(
    () => assertDeletableStagingReleasePrefix("tidebreak/releases/v0.4.2/", live),
    /production release/,
  );
  assert.throws(
    () => assertDeletableStagingReleasePrefix("tidebreak/latest.json", live),
    /production feed/,
  );
  assert.throws(
    () =>
      assertDeletableStagingReleasePrefix("tidebreak/staging/latest.json", live),
    /feed object|not a deletable/,
  );
  assert.throws(
    () =>
      assertDeletableStagingReleaseKey("tidebreak/staging/manifest.json", live),
    /feed object/,
  );
  assert.throws(
    () =>
      assertDeletableStagingReleaseKey(
        `${prefix(live)}manifest.json`,
        live,
      ),
    /live staging prefix/,
  );
  assert.throws(
    () =>
      assertDeletableStagingReleaseKey(
        "tidebreak/staging/releases/v0.0.0-staging.11/foo/../latest.json",
        live,
      ),
    /unsafe path/,
  );
});

test("the prune CLI emits a plan from stdin prefixes", () => {
  const result = spawnSync(
    process.execPath,
    [script, "--plan", "--latest-version", "0.0.0-staging.3", "--keep", "2"],
    {
      encoding: "utf8",
      input: [
        prefix("0.0.0-staging.1"),
        prefix("0.0.0-staging.2"),
        prefix("0.0.0-staging.3"),
        prefix("0.0.0-staging.4"),
      ].join("\n"),
    },
  );
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {
    latestVersion: "0.0.0-staging.3",
    keepCount: 2,
    keep: [prefix("0.0.0-staging.3"), prefix("0.0.0-staging.4")],
    delete: [prefix("0.0.0-staging.1"), prefix("0.0.0-staging.2")],
    skipped: [],
  });
});

test("the prune CLI rejects deleting the live prefix", () => {
  const result = spawnSync(
    process.execPath,
    [
      script,
      "--assert-delete-prefix",
      prefix("0.0.0-staging.3"),
      "--latest-version",
      "0.0.0-staging.3",
    ],
    { encoding: "utf8" },
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /live staging prefix/);
});

test("the prune shell asserts each prefix before recursive delete", () => {
  assert.match(shell, /node "\$planner" --plan --latest-version/);
  assert.match(shell, /--assert-delete-prefix "\$prefix" --latest-version/);
  const assertAt = shell.indexOf("--assert-delete-prefix");
  const rmAt = shell.indexOf("aws s3 rm");
  assert.ok(assertAt !== -1 && rmAt !== -1 && assertAt < rmAt);
  const dryRunAt = shell.indexOf('"$dry_run" = true');
  assert.ok(dryRunAt !== -1 && dryRunAt < rmAt);
  // Re-read live latest.json inside the delete loop so a concurrent publish
  // that advances the feed cannot delete the new current prefix.
  const deleteLoopAt = shell.indexOf("while IFS= read -r prefix");
  assert.notEqual(deleteLoopAt, -1);
  const rereadSlice = shell.slice(deleteLoopAt);
  const rereadAt = rereadSlice.indexOf("tidebreak/staging/latest.json");
  const loopAssertAt = rereadSlice.indexOf("--assert-delete-prefix");
  const loopRmAt = rereadSlice.indexOf("aws s3 rm");
  assert.ok(
    rereadAt !== -1 &&
      loopAssertAt !== -1 &&
      loopRmAt !== -1 &&
      rereadAt < loopAssertAt &&
      loopAssertAt < loopRmAt,
  );
  assert.match(rereadSlice, /jq -r '\.delete\[]'/);
  assert.match(shell, /s3:\/\/\$bucket\/\$prefix/);
  assert.doesNotMatch(shell, /[ "'`]tidebreak\/latest\.json/);
  assert.doesNotMatch(shell, /[ "'`]tidebreak\/releases\//);
  assert.match(shell, /tidebreak\/staging\/latest\.json/);
  assert.match(shell, /tidebreak\/staging\/releases\//);
  assert.match(shell, /--delimiter \//);
  assert.doesNotMatch(shell, /--include|--exclude|s3api delete-object/);
});
