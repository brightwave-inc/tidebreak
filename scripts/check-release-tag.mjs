#!/usr/bin/env node

import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const RELEASE_TAG = /^v((0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*))$/;

export function parseReleaseTag(tag) {
  if (typeof tag !== "string") return null;
  const match = RELEASE_TAG.exec(tag);
  if (!match || match[1] === "0.0.0") return null;
  return {
    version: match[1],
    major: match[2],
    minor: match[3],
    patch: match[4],
  };
}

function main() {
  const tag = process.argv[2];
  const parsed = parseReleaseTag(tag);
  if (!parsed) {
    throw new Error(
      `invalid release tag ${JSON.stringify(tag)}; expected vMAJOR.MINOR.PATCH (and not v0.0.0)`,
    );
  }
  const output = process.env.GITHUB_OUTPUT;
  if (output) {
    appendFileSync(
      output,
      `version=${parsed.version}\nmajor=${parsed.major}\nminor=${parsed.minor}\npatch=${parsed.patch}\n`,
    );
  }
  console.log(`release tag ${tag} selects Tidebreak ${parsed.version}`);
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
