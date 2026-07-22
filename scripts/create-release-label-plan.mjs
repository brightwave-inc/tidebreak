#!/usr/bin/env node

import { pathToFileURL } from "node:url";

import {
  MANAGED_RELEASE_LABELS,
  releaseLabels,
} from "./release-label.mjs";

const managedLabels = new Set(MANAGED_RELEASE_LABELS);

function currentManagedLabels(labels) {
  return labels
    .map((label) => (typeof label === "string" ? label : label?.name))
    .filter((label) => managedLabels.has(label))
    .sort();
}

export function createReleaseLabelPlan(pullRequests) {
  if (!Array.isArray(pullRequests)) {
    throw new Error("merged pull requests must be a JSON array");
  }

  const updates = [];
  const skipped = [];
  let unchanged = 0;

  for (const pullRequest of pullRequests) {
    const { number, title, labels } = pullRequest;
    if (!Number.isInteger(number) || number <= 0) {
      throw new Error(`invalid pull request number: ${number}`);
    }
    if (typeof title !== "string" || !Array.isArray(labels)) {
      throw new Error(`invalid pull request payload for #${number}`);
    }

    let desiredLabels;
    try {
      desiredLabels = releaseLabels(title).sort();
    } catch (error) {
      skipped.push({ number, title, reason: error.message });
      continue;
    }

    const existingLabels = currentManagedLabels(labels);
    const addLabels = desiredLabels.filter(
      (label) => !existingLabels.includes(label),
    );
    const removeLabels = existingLabels.filter(
      (label) => !desiredLabels.includes(label),
    );
    if (addLabels.length === 0 && removeLabels.length === 0) {
      unchanged += 1;
      continue;
    }
    updates.push({
      number,
      title,
      desired_labels: desiredLabels,
      add_labels: addLabels,
      remove_labels: removeLabels,
    });
  }

  return {
    summary: {
      total: pullRequests.length,
      classified: pullRequests.length - skipped.length,
      updates: updates.length,
      unchanged,
      skipped: skipped.length,
    },
    updates,
    skipped,
  };
}

async function main() {
  let input = "";
  for await (const chunk of process.stdin) input += chunk;
  const plan = createReleaseLabelPlan(JSON.parse(input));
  console.log(JSON.stringify(plan, null, 2));
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  await main();
}
