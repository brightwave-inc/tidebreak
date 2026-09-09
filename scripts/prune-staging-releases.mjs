#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

import { assertHostedUnderChannel } from "./desktop-channel.mjs";
import {
  compareStagingVersions,
  parseStagingVersion,
} from "./staging-version.mjs";

export const STAGING_RELEASES_PREFIX = "tidebreak/staging/releases/";
export const DEFAULT_STAGING_RELEASE_KEEP_COUNT = 3;
export const STAGING_FEED_KEYS = Object.freeze([
  "tidebreak/staging/latest.json",
  "tidebreak/staging/manifest.json",
]);

const STAGING_RELEASE_PREFIX_PATTERN =
  /^tidebreak\/staging\/releases\/v(0\.0\.0-staging\.[1-9]\d*)\/$/;
const STAGING_RELEASE_OBJECT_PATTERN =
  /^tidebreak\/staging\/releases\/v(0\.0\.0-staging\.[1-9]\d*)\/(.+)$/;

export function stagingReleasePrefix(version) {
  const parsed = parseStagingVersion(version);
  if (!parsed) {
    throw new Error(`invalid staging version: ${version}`);
  }
  return `${STAGING_RELEASES_PREFIX}v${parsed.version}/`;
}

export function parseStagingReleasePrefix(prefix) {
  if (typeof prefix !== "string") return null;
  const match = STAGING_RELEASE_PREFIX_PATTERN.exec(prefix);
  if (!match) return null;
  return parseStagingVersion(match[1]);
}

function assertSafeStagingKey(key) {
  if (typeof key !== "string" || key.length === 0) {
    throw new Error("staging delete key is missing");
  }
  if (key.includes("//") || key.split("/").includes("..")) {
    throw new Error(`refusing to delete staging key with an unsafe path: ${key}`);
  }
  if (STAGING_FEED_KEYS.includes(key)) {
    throw new Error(`refusing to delete staging feed object: ${key}`);
  }
  assertHostedUnderChannel(key, "staging");
}

export function assertDeletableStagingReleasePrefix(prefix, latestVersion) {
  assertSafeStagingKey(prefix);
  if (!parseStagingReleasePrefix(prefix)) {
    throw new Error(`not a deletable staging release prefix: ${prefix}`);
  }
  const livePrefix = stagingReleasePrefix(latestVersion);
  if (prefix === livePrefix) {
    throw new Error(`refusing to delete the live staging prefix ${prefix}`);
  }
}

export function assertDeletableStagingReleaseKey(key, latestVersion) {
  assertSafeStagingKey(key);
  const match = STAGING_RELEASE_OBJECT_PATTERN.exec(key);
  if (!match) {
    throw new Error(`not a deletable staging release object: ${key}`);
  }
  if (latestVersion !== undefined) {
    const livePrefix = stagingReleasePrefix(latestVersion);
    if (key.startsWith(livePrefix)) {
      throw new Error(
        `refusing to delete object from the live staging prefix: ${key}`,
      );
    }
  }
}

function parseKeepCount(keep) {
  const keepCount =
    typeof keep === "number" ? keep : Number.parseInt(keep, 10);
  if (!Number.isInteger(keepCount) || keepCount < 1) {
    throw new Error(`invalid keep count: ${keep}`);
  }
  return keepCount;
}

export function planStagingReleasePrune({
  latestVersion,
  prefixes,
  keep = DEFAULT_STAGING_RELEASE_KEEP_COUNT,
}) {
  const parsedLatest = parseStagingVersion(latestVersion);
  if (!parsedLatest) {
    throw new Error(`invalid staging version: ${latestVersion}`);
  }
  const keepCount = parseKeepCount(keep);
  const livePrefix = stagingReleasePrefix(parsedLatest.version);

  const unique = [
    ...new Set(
      (prefixes ?? [])
        .filter((prefix) => typeof prefix === "string")
        .map((prefix) => prefix.trim())
        .filter(Boolean),
    ),
  ];

  const skipped = [];
  const known = [];
  for (const prefix of unique) {
    const parsed = parseStagingReleasePrefix(prefix);
    if (!parsed) {
      skipped.push(prefix);
      continue;
    }
    known.push({ prefix, version: parsed.version });
  }
  known.sort((left, right) =>
    compareStagingVersions(right.version, left.version),
  );
  skipped.sort();

  const keepExisting = [];
  const keepSet = new Set();
  const liveEntry = known.find((entry) => entry.prefix === livePrefix);
  if (liveEntry) {
    keepExisting.push(liveEntry.prefix);
    keepSet.add(liveEntry.prefix);
  }
  for (const entry of known) {
    if (keepExisting.length >= keepCount) break;
    if (keepSet.has(entry.prefix)) continue;
    keepExisting.push(entry.prefix);
    keepSet.add(entry.prefix);
  }

  return {
    latestVersion: parsedLatest.version,
    keepCount,
    keep: keepExisting,
    delete: known
      .filter((entry) => !keepSet.has(entry.prefix))
      .sort((left, right) =>
        compareStagingVersions(left.version, right.version),
      )
      .map((entry) => entry.prefix),
    skipped,
  };
}

function requiredOption(args, name) {
  const index = args.indexOf(`--${name}`);
  if (index === -1 || args[index + 1] === undefined) {
    throw new Error(`missing --${name}`);
  }
  return args[index + 1];
}

function main() {
  const args = process.argv.slice(2);
  const command = args[0];
  if (command === "--plan") {
    const rest = args.slice(1);
    const keepIndex = rest.indexOf("--keep");
    const keep =
      keepIndex === -1
        ? DEFAULT_STAGING_RELEASE_KEEP_COUNT
        : rest[keepIndex + 1];
    const prefixes = readFileSync(0, "utf8")
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
    const plan = planStagingReleasePrune({
      latestVersion: requiredOption(rest, "latest-version"),
      prefixes,
      keep,
    });
    console.log(JSON.stringify(plan, null, 2));
    return;
  }
  if (command === "--assert-delete-prefix") {
    assertDeletableStagingReleasePrefix(
      args[1],
      requiredOption(args, "latest-version"),
    );
    return;
  }
  if (command === "--assert-delete-key") {
    const latestIndex = args.indexOf("--latest-version");
    const latestVersion =
      latestIndex === -1 ? undefined : args[latestIndex + 1];
    assertDeletableStagingReleaseKey(args[1], latestVersion);
    return;
  }
  throw new Error(
    "usage: prune-staging-releases.mjs --plan --latest-version <version> [--keep <n>] | --assert-delete-prefix <prefix> --latest-version <version> | --assert-delete-key <key> [--latest-version <version>]",
  );
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
