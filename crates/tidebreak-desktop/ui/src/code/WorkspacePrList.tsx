import { GitPullRequest } from "lucide-react";

import type {
  CodeWorkspacePullRequestFact,
  PullRequestDigest,
} from "../api/types";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import {
  PR_CHIP_TONE_CLASSES,
  PR_ICON_TONE_CLASSES,
  prTone,
  prToneLabel,
} from "./workspaceCards";

/**
 * Every pull request attributed to the workspace (decision 62), as a compact
 * selector above the single-PR panel. Rendered only when there is more than
 * one, so the common one-PR workspace keeps today's surface untouched.
 */
export function WorkspacePrList({
  items,
  selectedNumber,
  onSelect,
}: {
  items: readonly CodeWorkspacePullRequestFact[];
  /** The pull request the panel below is showing. */
  selectedNumber?: number;
  onSelect: (item: CodeWorkspacePullRequestFact) => void;
}) {
  return (
    <nav
      aria-label="Pull requests this workspace worked on"
      className="border-border-subtle flex flex-col gap-0.5 border-b px-2 pt-1.5 pb-2"
    >
      {items.map((item) => {
        const tone = prTone(item);
        const selected = item.number === selectedNumber;
        return (
          <button
            key={`${item.host}/${item.repo_owner}/${item.repo_name}#${item.number}`}
            type="button"
            aria-current={selected ? "true" : undefined}
            aria-label={`Pull request #${item.number}: ${item.title}, ${prToneLabel(item)}${
              item.relation === "authored" ? ", authored here" : ""
            }`}
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
              className={cn("size-3.5 shrink-0", PR_ICON_TONE_CLASSES[tone])}
              aria-hidden
            />
            <span className="text-muted-foreground shrink-0 tabular-nums">
              #{item.number}
            </span>
            <span className="min-w-0 flex-1 truncate">{item.title}</span>
            <span className="text-muted-foreground shrink-0 truncate text-[10px]">
              {item.repo_owner}/{item.repo_name}
            </span>
            <span
              className={cn(
                "shrink-0 rounded-full px-1.5 py-px text-[10px] font-medium",
                PR_CHIP_TONE_CLASSES[tone],
              )}
            >
              {prToneLabel(item)}
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
