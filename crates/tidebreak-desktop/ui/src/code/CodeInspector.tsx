import { GitPullRequest, MessageSquare, X } from "lucide-react";

import type { ApiClient } from "../api/client";
import type {
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";
import { openExternal } from "@/host";
import { PrCard } from "./PrCard";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

/**
 * Workspace review rail: git sync, pull-request state, and comments.
 *
 * The conversation stays the center of the workspace. Commit, push, the PR,
 * and review discussion live here so they are always one chord away without
 * sitting on top of the transcript.
 */
export function CodeInspector({
  client,
  workspaceId,
  workspace,
  contentRevision,
  shortcutHint,
  onClose,
}: {
  client: ApiClient;
  workspaceId: string;
  workspace: CodeWorkspaceSnapshot | null;
  contentRevision: number;
  shortcutHint?: string;
  onClose: () => void;
}) {
  const digest = useCodeUpdatesStore((state) => state.byWorkspace[workspaceId]);
  const pr = digest?.pr_state ?? workspace?.pr;
  const active = workspace?.status !== "archived";

  return (
    <aside
      className="flex w-80 shrink-0 flex-col overflow-hidden border-l"
      aria-label="Review"
      data-testid="code-inspector"
    >
      <header className="flex h-10 shrink-0 items-center gap-2 border-b px-3">
        <GitPullRequest className="text-muted-foreground size-3.5 shrink-0" />
        <h2 className="min-w-0 flex-1 truncate text-sm font-medium">Review</h2>
        <WithTooltip label={shortcutHint ? `Hide sidebar ${shortcutHint}` : "Hide sidebar"}>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label="Hide review sidebar"
            onClick={onClose}
          >
            <X />
          </Button>
        </WithTooltip>
      </header>
      <div className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto px-3 py-3">
        {active && (
          <PrCard
            client={client}
            workspaceId={workspaceId}
            contentRevision={contentRevision}
            framed={false}
          />
        )}
        <ReviewComments pr={pr} />
      </div>
    </aside>
  );
}

function ReviewComments({ pr }: { pr?: PullRequestDigest }) {
  return (
    <section className="flex flex-col gap-2" aria-label="Comments">
      <header className="flex items-center gap-2">
        <MessageSquare className="text-muted-foreground size-3.5 shrink-0" />
        <h3 className="text-sm font-medium">Comments</h3>
      </header>
      {pr ? (
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap items-center gap-1.5">
            <Badge variant={prStateVariant(pr.state)} size="sm">
              #{pr.number} {pr.state}
            </Badge>
            {pr.checks_summary && (
              <Badge variant={checksVariant(pr.checks_summary)} size="sm">
                {pr.checks_summary}
              </Badge>
            )}
          </div>
          <p className="text-muted-foreground text-xs leading-relaxed">
            Review comments live on the pull request. Open it to read and reply.
          </p>
          {pr.url && (
            <Button
              type="button"
              size="sm"
              variant="secondary"
              className="self-start"
              onClick={() => void openExternal(pr.url!).catch(() => undefined)}
            >
              Open pull request
            </Button>
          )}
        </div>
      ) : (
        <p className="text-muted-foreground text-xs leading-relaxed">
          Comments and review discussion show up here once this workspace has a
          pull request.
        </p>
      )}
    </section>
  );
}

function prStateVariant(state: string): "success" | "warning" | "critical" | "info" | "outline" {
  const token = state.toLowerCase();
  if (token === "open") return "success";
  if (token === "merged") return "info";
  if (token === "closed") return "critical";
  return "outline";
}

function checksVariant(summary: string): "success" | "warning" | "critical" | "info" | "outline" {
  const lower = summary.toLowerCase();
  if (/\b[1-9]\d* failing/.test(lower)) return "critical";
  if (/\b[1-9]\d* pending/.test(lower)) return "warning";
  if (/\b[1-9]\d* passing/.test(lower) || lower.includes("passing")) return "success";
  return "outline";
}
