#!/usr/bin/env node

import { pathToFileURL } from "node:url";

import { parseReleaseTag } from "./check-release-tag.mjs";

function parseVersion(version) {
  const parsed = parseReleaseTag(`v${version}`);
  if (!parsed) throw new Error(`invalid release version: ${version}`);
  return [parsed.major, parsed.minor, parsed.patch].map(BigInt);
}

export function compareReleaseVersions(left, right) {
  const leftParts = parseVersion(left);
  const rightParts = parseVersion(right);
  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] < rightParts[index]) return -1;
    if (leftParts[index] > rightParts[index]) return 1;
  }
  return 0;
}

export function assertReleaseDoesNotRegress(current, candidate) {
  if (compareReleaseVersions(candidate, current) < 0) {
    throw new Error(
      `refusing to replace current release ${current} with older release ${candidate}`,
    );
  }
}

function main() {
  const [current, candidate] = process.argv.slice(2);
  if (!current || !candidate) {
    throw new Error("usage: check-release-order.mjs <current> <candidate>");
  }
  assertReleaseDoesNotRegress(current, candidate);
  console.log(`release ${candidate} may follow ${current}`);
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
