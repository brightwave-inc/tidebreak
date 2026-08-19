#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

export const WRITE_PERMISSIONS = new Set(["admin", "maintain", "write"]);

export function hasWritePermission(permission) {
  return WRITE_PERMISSIONS.has(permission);
}

const TERMINAL_REVIEW_STATES = new Set([
  "APPROVED",
  "CHANGES_REQUESTED",
  "DISMISSED",
]);

export function latestWriterReviews(reviews, permissionFor) {
  const latest = new Map();
  for (const review of reviews) {
    if (!TERMINAL_REVIEW_STATES.has(review.state)) continue;
    if (typeof review.user !== "string" || review.user.length === 0) continue;
    if (!hasWritePermission(permissionFor(review.user))) continue;
    latest.set(review.user, review);
  }
  return [...latest.values()];
}

export function evaluateEngineeringApproval({
  eventName,
  author,
  headSha,
  reviews,
  permissionFor,
}) {
  if (eventName === "merge_group") {
    return {
      ok: true,
      reason: "merge groups reuse the pull request approval check",
    };
  }

  if (typeof author !== "string" || author.length === 0) {
    return { ok: false, reason: "the pull request author is missing" };
  }

  if (hasWritePermission(permissionFor(author))) {
    return {
      ok: true,
      reason: `${author} can write to the repository, so this pull request does not need a separate approval`,
    };
  }

  if (typeof headSha !== "string" || headSha.length === 0) {
    return { ok: false, reason: "the pull request head SHA is missing" };
  }

  const approvers = latestWriterReviews(reviews, permissionFor)
    .filter((review) => review.state === "APPROVED")
    .filter((review) => review.commitId === headSha)
    .map((review) => review.user);
  if (approvers.length > 0) {
    return {
      ok: true,
      reason: `${approvers.join(", ")} approved ${headSha.slice(0, 7)}`,
    };
  }

  return {
    ok: false,
    reason: [
      `${author} cannot write to this repository.`,
      "An approving review from someone with write access is required on the current head commit.",
    ].join(" "),
  };
}

function ghApi(path, extraArgs = []) {
  return execFileSync("gh", ["api", ...extraArgs, path], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function permissionFor(repository, login) {
  try {
    const payload = JSON.parse(
      ghApi(`repos/${repository}/collaborators/${login}/permission`),
    );
    return typeof payload.permission === "string" ? payload.permission : "none";
  } catch {
    return "none";
  }
}

function reviewsFor(repository, number) {
  const raw = ghApi(`repos/${repository}/pulls/${number}/reviews`, [
    "--paginate",
    "--jq",
    ".[] | {state, commitId: .commit_id, user: .user.login}",
  ]);
  return raw
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .map((line) => JSON.parse(line));
}

function main() {
  const eventName = process.env.EVENT_NAME ?? "";
  if (eventName === "merge_group") {
    const decision = evaluateEngineeringApproval({
      eventName,
      author: "",
      headSha: "",
      reviews: [],
      permissionFor: () => "none",
    });
    console.log(decision.reason);
    return;
  }

  const repository = process.env.CANONICAL_REPOSITORY ?? "";
  const number = process.env.PR_NUMBER ?? "";
  const author = process.env.PR_AUTHOR ?? "";
  const headSha = process.env.PR_HEAD_SHA ?? "";
  if (!repository || !number || !author || !headSha) {
    console.error(
      "CANONICAL_REPOSITORY, PR_NUMBER, PR_AUTHOR, and PR_HEAD_SHA are required",
    );
    process.exitCode = 1;
    return;
  }

  const cache = new Map();
  const decision = evaluateEngineeringApproval({
    eventName,
    author,
    headSha,
    reviews: reviewsFor(repository, number),
    permissionFor: (login) => {
      if (!cache.has(login)) {
        cache.set(login, permissionFor(repository, login));
      }
      return cache.get(login);
    },
  });
  if (decision.ok) {
    console.log(decision.reason);
    return;
  }
  console.error(decision.reason);
  process.exitCode = 1;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main();
}
