#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

import { parseReleaseTag } from "./check-release-tag.mjs";
import { normalizeReleaseList } from "./reconcile-release-drafts.mjs";

export function versionTagsFromRefs(refs) {
  if (!Array.isArray(refs)) {
    throw new Error("tag refs must be a JSON array");
  }
  return [
    ...new Set(
      refs.flatMap((ref) => {
        if (typeof ref !== "string" || ref.length === 0) return [];
        const tag = ref.replace(/^refs\/tags\//, "").replace(/\^{}$/, "");
        return parseReleaseTag(tag) ? [tag] : [];
      }),
    ),
  ];
}

export function planReleaseBaseline({ releases, tags }) {
  const published = normalizeReleaseList(releases).filter(
    (release) => release?.draft === false,
  );
  const versionTags = versionTagsFromRefs(tags);

  if (published.length > 0) {
    return {
      action: "ok",
      published_count: published.length,
      version_tag_count: versionTags.length,
    };
  }

  if (versionTags.length > 0) {
    return {
      action: "fail",
      published_count: 0,
      version_tag_count: versionTags.length,
      reason:
        "version tags exist but no published GitHub Release was listed; refusing to invoke Release Drafter",
    };
  }

  return {
    action: "first_release",
    published_count: 0,
    version_tag_count: 0,
  };
}

function parseArgs(argv) {
  const args = { releases: null, tags: null };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (flag === "--releases") {
      args.releases = value;
      index += 1;
    } else if (flag === "--tags") {
      args.tags = value;
      index += 1;
    } else {
      throw new Error(`unknown argument: ${flag}`);
    }
  }
  if (!args.releases || !args.tags) {
    throw new Error(
      "usage: require-release-baseline.mjs --releases <path> --tags <path>",
    );
  }
  return args;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const plan = planReleaseBaseline({
    releases: JSON.parse(readFileSync(args.releases, "utf8")),
    tags: JSON.parse(readFileSync(args.tags, "utf8")),
  });
  console.log(JSON.stringify(plan));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
