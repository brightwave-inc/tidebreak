import type { ReactNode } from "react";
import {
  GitBranch,
  PanelRight,
  PanelRightClose,
  SquareTerminal,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { MiddleTruncate } from "./MiddleTruncate";

/**
 * Stable workspace chrome.
 *
 * The conversation and every working surface share this header. It keeps the
 * workspace identity calm on the left and the live Git / PR controls visible
 * on the right, instead of letting each pane grow its own toolbar.
 */
export function WorkspaceHeader({
  title,
  repoName,
  branchName,
  worktreePath,
  loading = false,
  workflow,
  sessionStatus,
  terminalOpen,
  reviewOpen,
  reviewUnavailableReason,
  terminalShortcut,
  reviewShortcut,
  onToggleTerminal,
  onToggleReview,
  overflowAction,
  className,
}: {
  title?: string;
  repoName?: string;
  branchName?: string;
  worktreePath?: string;
  loading?: boolean;
  /** Persistent Git / PR state and its primary action. */
  workflow?: ReactNode;
  /** Attention, approval, and harness state. */
  sessionStatus?: ReactNode;
  terminalOpen: boolean;
  reviewOpen: boolean;
  /** Why the review sidebar cannot open here, when it cannot. */
  reviewUnavailableReason?: string;
  terminalShortcut?: string;
  reviewShortcut?: string;
  onToggleTerminal: () => void;
  onToggleReview: () => void;
  overflowAction?: ReactNode;
  className?: string;
}) {
  const tooltip = [title, repoName, branchName, worktreePath]
    .filter(Boolean)
    .join("\n");

  return (
    <header
      className={cn(
        "workspace-header flex min-h-14 min-w-0 shrink-0 flex-wrap items-center gap-x-2 gap-y-1.5 border-b border-border-subtle bg-background/95 px-2.5 py-1.5 backdrop-blur-sm min-[1100px]:flex-nowrap min-[1100px]:gap-3 min-[1100px]:px-3.5 min-[1100px]:py-0",
        className,
      )}
      data-testid="workspace-header"
    >
      <div className="min-w-0 flex-1 py-1 min-[1100px]:py-2">
        {loading ? (
          <div
            data-testid="workspace-header-skeleton"
            className="flex min-w-0 flex-col gap-1.5"
            aria-label="Loading workspace"
          >
            <Skeleton className="h-3.5 w-40" />
            <Skeleton className="h-2.5 w-52" />
          </div>
        ) : (
          <>
            <h1
              className="truncate text-md font-semibold tracking-[-0.01em]"
              title={tooltip || undefined}
            >
              {title}
            </h1>
            {(repoName || branchName) && (
              <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                {repoName && <span className="shrink-0">{repoName}</span>}
                {repoName && branchName && (
                  <span className="text-border" aria-hidden>
                    /
                  </span>
                )}
                {branchName && (
                  <span className="flex min-w-0 items-center gap-1 font-mono">
                    <GitBranch className="size-3 shrink-0" aria-hidden />
                    <MiddleTruncate text={branchName} className="min-w-0" />
                  </span>
                )}
              </div>
            )}
          </>
        )}
      </div>

      <div
        className="flex shrink-0 items-center gap-1 min-[1100px]:gap-1.5"
        data-testid="workspace-header-utilities"
      >
        <div className="mx-0.5 hidden h-4 w-px bg-border-subtle min-[1100px]:block" />
        <WithTooltip
          label={
            terminalOpen
              ? `Go to terminal${shortcutSuffix(terminalShortcut)}`
              : `New terminal${shortcutSuffix(terminalShortcut)}`
          }
        >
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            className="rounded-lg"
            aria-pressed={terminalOpen}
            aria-label="Terminal"
            onClick={onToggleTerminal}
          >
            <SquareTerminal />
          </Button>
        </WithTooltip>
        <WithTooltip
          label={
            reviewUnavailableReason
              ? reviewUnavailableReason
              : reviewOpen
                ? `Hide review sidebar${shortcutSuffix(reviewShortcut)}`
                : `Review sidebar${shortcutSuffix(reviewShortcut)}`
          }
        >
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            className="rounded-lg aria-disabled:cursor-default aria-disabled:opacity-50"
            aria-pressed={reviewOpen}
            aria-label="Review sidebar"
            aria-disabled={reviewUnavailableReason ? true : undefined}
            onClick={reviewUnavailableReason ? undefined : onToggleReview}
          >
            {reviewOpen ? <PanelRightClose /> : <PanelRight />}
          </Button>
        </WithTooltip>
        {overflowAction}
      </div>

      {(workflow || sessionStatus) && (
        <div
          className="flex w-full min-w-0 items-center gap-1.5 pb-0.5 min-[1100px]:w-auto min-[1100px]:pb-0"
          data-testid="workspace-header-status"
          role="group"
          aria-label="Workspace status and workflow"
        >
          {workflow}
          {sessionStatus && (
            <div
              className={cn(
                "flex min-w-0 items-center gap-1 overflow-hidden",
                workflow && "ml-auto min-[1100px]:ml-0",
              )}
            >
              {sessionStatus}
            </div>
          )}
        </div>
      )}
    </header>
  );
}

function shortcutSuffix(shortcut?: string): string {
  return shortcut ? ` ${shortcut}` : "";
}
