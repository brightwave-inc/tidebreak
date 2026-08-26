import { GitPullRequest } from "lucide-react";

import type {
  CodeWorkspacePullRequestFact,
  PullRequestDigest,
} from "../api/types";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import {
  PULL_REQUEST_LIFECYCLE_LABEL,
  PULL_REQUEST_LIFECYCLE_TONE,
  pullRequestLifecycle,
} from "./prState";
import { STATUS_CHIP, STATUS_MARK } from "./statusTone";

/**
 * One fact's full identity. Numbers repeat across repositories, so every
 * selection and highlight decision keys on this, never on the number.
 */
export function factKey(fact: CodeWorkspacePullRequestFact): string {
  return `${fact.host}/${fact.repo_owner}/${fact.repo_name}#${fact.number}`;
}

/**
 * Every pull request attributed to the workspace (decision 62), as a compact
 * selector above the single-PR panel. Rendered only when there is more than
 * one, so the common one-PR workspace keeps today's surface untouched.
 */
export function WorkspacePrList({
  items,
  selectedKey,
  onSelect,
}: {
  items: readonly CodeWorkspacePullRequestFact[];
  /** Full identity of the pull request the panel below is showing. */
  selectedKey?: string;
  onSelect: (item: CodeWorkspacePullRequestFact) => void;
}) {
  return (
    <nav
      aria-label="Pull requests this workspace worked on"
      className="border-border-subtle flex flex-col gap-0.5 border-b px-2 pt-1.5 pb-2"
    >
      {items.map((item) => {
        const lifecycle = pullRequestLifecycle(item);
        const selected = factKey(item) === selectedKey;
        return (
          <button
            key={factKey(item)}
            type="button"
            aria-current={selected ? "true" : undefined}
            aria-label={`Pull request #${item.number}: ${item.title}, ${
              PULL_REQUEST_LIFECYCLE_LABEL[lifecycle]
            }${item.relation === "authored" ? ", authored here" : ""}`}
            onClick={() => onSelect(item)}
            className={cn(
              "flex min-w-0 items-center gap-1.5 rounded-md px-1.5 py-1 text-left text-xs",
              HOVER_TINT,
              FOCUS_RING_TIGHT,
              selected &&
                "bg-background shadow-[inset_0_0_0_1px_var(--border-subtle)]",
            )}
          >
            <GitPullRequest
              className={cn(
                "size-3.5 shrink-0",
                STATUS_MARK[PULL_REQUEST_LIFECYCLE_TONE[lifecycle]],
              )}
              aria-hidden
            />
            <span className="text-muted-foreground shrink-0 tabular-nums">
              #{item.number}
            </span>
            <span className="min-w-0 flex-1 truncate">{item.title}</span>
            <span className="text-muted-foreground shrink-0 truncate text-2xs">
              {item.repo_owner}/{item.repo_name}
            </span>
            <span
              className={cn(
                "shrink-0 rounded-full px-1.5 py-px text-2xs font-medium",
                STATUS_CHIP[PULL_REQUEST_LIFECYCLE_TONE[lifecycle]],
              )}
            >
              {PULL_REQUEST_LIFECYCLE_LABEL[lifecycle]}
            </span>
          </button>
        );
      })}
    </nav>
  );
}

/**
 * Project a stored fact into the digest shape the single-PR panel reads.
 * A snapshot view: checks, review, and mergeability stay absent, since the
 * fact store deliberately never carries them.
 */
export function digestFromFact(
  fact: CodeWorkspacePullRequestFact,
): PullRequestDigest {
  return {
    number: fact.number,
    url: fact.url,
    state: fact.state,
    title: fact.title,
    draft: fact.draft,
    merged: fact.state === "merged",
    head_branch: fact.head_branch,
    base_branch: fact.base_branch,
    ...(fact.head_sha !== undefined ? { head_sha: fact.head_sha } : {}),
  };
}
