#!/usr/bin/env node

import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const STAGING_VERSION = /^0\.0\.0-staging\.([1-9]\d*)$/;

export function parseStagingVersion(version) {
  if (typeof version !== "string") return null;
  const match = STAGING_VERSION.exec(version);
  if (!match) return null;
  return {
    version,
    build: match[1],
  };
}

export function stagingVersionFromRunNumber(runNumber) {
  const value =
    typeof runNumber === "number" ? runNumber : Number.parseInt(runNumber, 10);
  if (!Number.isInteger(value) || value < 1) {
    throw new Error(`invalid staging run number: ${runNumber}`);
  }
  return `0.0.0-staging.${value}`;
}

export function stagingTag(version) {
  const parsed = parseStagingVersion(version);
  if (!parsed) {
    throw new Error(`invalid staging version: ${version}`);
  }
  return `staging-${parsed.build}`;
}

export function compareStagingVersions(left, right) {
  const leftParsed = parseStagingVersion(left);
  const rightParsed = parseStagingVersion(right);
  if (!leftParsed || !rightParsed) {
    throw new Error(`invalid staging version: ${leftParsed ? right : left}`);
  }
  const leftBuild = BigInt(leftParsed.build);
  const rightBuild = BigInt(rightParsed.build);
  if (leftBuild < rightBuild) return -1;
  if (leftBuild > rightBuild) return 1;
  return 0;
}

export function assertStagingDoesNotRegress(current, candidate) {
  if (compareStagingVersions(candidate, current) < 0) {
    throw new Error(
      `refusing to replace current staging ${current} with older staging ${candidate}`,
    );
  }
}

function writeOutput(parsed) {
  const output = process.env.GITHUB_OUTPUT;
  if (!output) return;
  appendFileSync(
    output,
    `version=${parsed.version}\ntag=${stagingTag(parsed.version)}\nbuild=${parsed.build}\n`,
  );
}

function main() {
  const args = process.argv.slice(2);
  if (args[0] === "--from-run-number") {
    const version = stagingVersionFromRunNumber(args[1]);
    const parsed = parseStagingVersion(version);
    writeOutput(parsed);
    console.log(version);
    return;
  }
  if (args[0] === "--assert-not-regress") {
    assertStagingDoesNotRegress(args[1], args[2]);
    console.log(`staging ${args[2]} may follow ${args[1]}`);
    return;
  }
  const version = args[0];
  const parsed = parseStagingVersion(version);
  if (!parsed) {
    throw new Error(
      `invalid staging version ${JSON.stringify(version)}; expected 0.0.0-staging.N`,
    );
  }
  writeOutput(parsed);
  console.log(`staging version ${parsed.version} is build ${parsed.build}`);
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
