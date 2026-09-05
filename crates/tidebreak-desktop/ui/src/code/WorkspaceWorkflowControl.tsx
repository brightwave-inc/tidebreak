import { useEffect, useId, useMemo, useState } from "react";
import {
  ArrowUpRight,
  ChevronDown,
  CircleDotDashed,
  GitBranch,
  RefreshCw,
} from "lucide-react";
import { toast } from "sonner";

import { HttpError, type ApiClient } from "../api/client";
import type {
  CodePrMergeMethod,
  CodeWatchState,
  PullRequestDigest,
} from "../api/types";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ConfirmDialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Spinner } from "@/components/ui/spinner";
import { openExternal } from "@/host";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { fetchFixErrorsLogs } from "./checkLogs";
import {
  prDirectMergeAction,
  prWorkflowPrompt,
  type PrPromptAction,
} from "./prActions";
import { useCodeUiStore } from "./CodeUiStore";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";
import {
  composePrPrompt,
  workspaceActionPrompt,
  resolveWorkflowShortcut,
  workspaceMergeConflictMessage,
  workspaceMergeIdentity,
  workspaceWorkflowActionLabel,
  workspaceWorkflowModel,
  type WorkspaceWorkflowAction,
} from "./workspaceWorkflow";
import { STATUS_MARK, STATUS_TEXT } from "./statusTone";
import {
  WorkspaceStatusDetails,
  workspaceStatusLabel,
} from "./WorkspaceStatusDetails";

/**
 * Compact workspace workflow control for the top chrome.
 *
 * The status segment explains the current Git/PR state. The primary segment
 * advances the workflow, while the chevron keeps secondary actions nearby
 * without turning the header into another toolbar.
 */
export function WorkspaceWorkflowControl({
  client,
  workspaceId,
  branchName,
  baseRef,
  fallbackPr,
  resource,
  onOpenSourceControl,
  onArchive,
  onOpenPr,
  onOpenWatchTask,
}: {
  client: Pick<
    ApiClient,
    | "pushCodeWorkspace"
    | "createCodePullRequest"
    | "markCodePrReady"
    | "mergeCodePr"
    | "startCodeWatch"
    | "stopCodeWatch"
    | "writeCodeCheckLogs"
  >;
  workspaceId: string;
  branchName: string;
  baseRef?: string;
  fallbackPr?: PullRequestDigest;
  resource: CodeWorkspacePrResource;
  onOpenSourceControl: () => void;
  onArchive?: () => void;
  /** Open the pull request as a workspace center tab. */
  onOpenPr?: () => void;
  /** Open the watch task's transcript; the segment is a link to the fork. */
  onOpenWatchTask?: () => void;
}) {
  const [detailsOpen, setDetailsOpen] = useState(false);
  // Downloading the failing job logs is a host read the reader waits on, so
  // the primary button spins through it rather than looking dead.
  const [attachingLogs, setAttachingLogs] = useState(false);
  const popoverTitleId = useId();
  const { confirm, dialog: confirmDialog } = useConfirm();
  const runComposerPrompt = useCodeUiStore((state) => state.runComposerPrompt);
  const workflowShortcutPending = useCodeUiStore(
    (state) => state.workflowShortcutPending,
  );
  const agentActionRunning = useCodeUiStore(
    (state) => state.composerActionScope !== null,
  );
  const model = useMemo(
    () => workspaceWorkflowModel(resource.data, fallbackPr),
    [fallbackPr, resource.data],
  );
  const watch = resource.data?.watch;
  const watchActive =
    watch !== undefined &&
    (watch.state === "watching" ||
      watch.state === "fixing" ||
      watch.state === "blocked");
  const primary =
    watchActive || resource.error || (model.primary === "archive" && !onArchive)
      ? undefined
      : model.primary;
  const busy = resource.busy;
  const primaryLabel = primary
    ? workspaceWorkflowActionLabel(primary, model.stage)
    : null;
  const statusLabel = model.pr
    ? model.summary.replace(/^#\d+\s*·\s*/, "")
    : model.summary;
  const detailsTitle =
    model.pr && resource.mutationError?.includes("Refresh workspace status")
      ? `Pull request #${model.pr.number} changed`
      : model.title;

  // Republish the primary action for the command palette, which leads with it.
  // Cleared on unmount so the palette never offers a step for a workspace the
  // reader has already navigated away from.
  const publishWorkflowSuggestion = useCodeUiStore(
    (state) => state.publishWorkflowSuggestion,
  );
  useEffect(() => {
    publishWorkflowSuggestion(
      primaryLabel
        ? {
            workspaceId,
            label: primaryLabel,
            summary: statusLabel,
            tone: model.tone,
          }
        : null,
    );
    return () => publishWorkflowSuggestion(null);
  }, [
    publishWorkflowSuggestion,
    workspaceId,
    primaryLabel,
    statusLabel,
    model.tone,
  ]);
  // While a watch runs, agent actions would contend for the same worktree;
  // local Git and navigation actions stay available.
  const secondaryActions = watchActive
    ? model.secondary.filter(
        (action) =>
          action === "open_pr" ||
          action === "open_source" ||
          action === "push" ||
          action === "create_pr",
      )
    : model.secondary;

  /**
   * Carry out a Ship chord raised by the shell.
   *
   * Taken here rather than run there because the chord's meaning depends on the
   * branch and pull-request state this control already holds. Effects run after
   * the render that saw the request, so the model this resolves against is the
   * current one. Taking the request is what keeps a remount from repeating it.
   */
  useEffect(() => {
    if (workflowShortcutPending?.workspaceId !== workspaceId) return;
    const shortcut = useCodeUiStore
      .getState()
      .takeWorkflowShortcut(workspaceId);
    if (shortcut === null) return;
    if (resource.error && !["view_pr", "source_control"].includes(shortcut)) {
      toast.error("Refresh workspace status before acting on it");
      return;
    }
    const resolution = resolveWorkflowShortcut(shortcut, model, watchActive);
    if ("blocked" in resolution) {
      toast.message(resolution.blocked);
      return;
    }
    if ("stopWatch" in resolution) {
      void stopWatch();
      return;
    }
    if ("autoMerge" in resolution) {
      if (busy !== null) {
        toast.message("Another workspace action is already running");
        return;
      }
      void mergePr(true);
      return;
    }
    if (busy !== null || agentActionRunning || attachingLogs) {
      toast.message("Another workspace action is already running");
      return;
    }
    void run(resolution.run);
    // Only the request re-runs this. The rest is state the effect reads at the
    // moment the chord arrives; listing it would re-fire on every status poll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workflowShortcutPending, workspaceId]);

  /**
   * Hand one prepared prompt to the workspace's agent.
   *
   * Fix-errors takes a detour first: the server downloads the failing jobs'
   * logs so the prompt can name files the agent reads, instead of asking it to
   * go and find them. The scope is claimed before the download so a second
   * press cannot start a second fetch, and a failed fetch still sends the
   * prompt.
   */
  async function runAgentAction(action: PrPromptAction) {
    const pr = model.pr;
    if (!pr) return;
    setDetailsOpen(false);
    if (action !== "fix_errors") {
      runComposerPrompt(workspaceId, prWorkflowPrompt(action, pr));
      return;
    }
    if (attachingLogs || agentActionRunning) return;
    setAttachingLogs(true);
    try {
      const logs = await fetchFixErrorsLogs(client, workspaceId);
      runComposerPrompt(workspaceId, prWorkflowPrompt(action, pr, logs));
    } finally {
      setAttachingLogs(false);
    }
  }

  /**
   * Merge through the user-initiated endpoint, not through the agent.
   *
   * Decision 42 reserves merging for the user: the general `gh` runner refuses
   * merge argv, and only `POST /code/workspaces/{id}/pr/merge` reaches the
   * runner that allows it. Asking an agent to merge would route around that,
   * so this button and its chord call the endpoint the review sidebar's Merge
   * button calls.
   *
   * Squash is the default the review sidebar opens on, and the confirmation
   * names the method, so the reader sees which one before it lands. Pick a
   * different one from the review sidebar.
   */
  async function mergePr(auto = false, method: CodePrMergeMethod = "squash") {
    const pr = model.pr;
    if (!pr) return;
    const exact = workspaceMergeIdentity(pr);
    if (!exact) {
      setDetailsOpen(true);
      resource.setMutationError(
        "Refresh workspace status before merging. The pull request identity or head commit is incomplete.",
      );
      return;
    }
    setDetailsOpen(false);
    const base = pr.base_branch ?? "its base branch";
    // Two forms of the same verb: the immediate merge reads as something done
    // to the pull request, the armed one as something GitHub will do.
    const [landed, lands] =
      method === "squash"
        ? ["squash-merged", "squash-merges"]
        : method === "rebase"
          ? ["rebased and merged", "rebases and merges"]
          : ["merged", "merges"];
    const action = prDirectMergeAction(pr);
    const kind = auto
      ? action?.kind === "merge_when_ready"
        ? "merge_when_ready"
        : "enable_auto_merge"
      : "merge";
    const confirmLabel =
      kind === "merge_when_ready"
        ? "Merge when ready"
        : kind === "enable_auto_merge"
          ? "Enable auto-merge"
          : "Merge";
    const ok = await confirm({
      title:
        kind === "merge_when_ready"
          ? `Merge #${pr.number} when ready?`
          : kind === "enable_auto_merge"
            ? `Enable auto-merge on #${pr.number}?`
            : `Merge #${pr.number}?`,
      description:
        kind === "merge"
          ? `The pull request is ${landed} into ${base} on GitHub. Tidebreak checks the workspace and pull request again before it sends the merge.`
          : kind === "merge_when_ready"
            ? `GitHub adds the pull request to the merge queue and ${lands} it into ${base} once the remaining requirements pass. Tidebreak checks the workspace and pull request again first.`
            : `GitHub ${lands} the pull request into ${base} once the remaining requirements pass. Tidebreak checks the workspace and pull request again first.`,
      confirmLabel,
    });
    if (!ok) return;
    try {
      const next = await resource.runMutation(
        auto ? "auto_merge" : "merge",
        () => client.mergeCodePr(workspaceId, { ...exact, method, auto }),
      );
      if (!next) return;
      resource.adopt(next);
      toast.success(
        kind === "merge_when_ready"
          ? "Merge when ready armed"
          : kind === "enable_auto_merge"
            ? "Auto-merge enabled"
            : "Merged",
      );
    } catch (err) {
      const refreshable = workspaceMergeConflictMessage(err);
      // Keep the pull request context and refresh control visible when local
      // or host state changed after confirmation.
      if (refreshable) setDetailsOpen(true);
      const message = refreshable
        ? refreshable
        : err instanceof HttpError && err.kind === "pr_not_mergeable"
          ? err.message
          : friendlyErrorMessage(err, "Could not merge");
      resource.setMutationError(message);
      toast.error(message);
    }
  }

  /**
   * Take the pull request out of draft through the user-initiated endpoint.
   *
   * Readying a draft is a pull-request state change, which decision 42 keeps
   * off the agent path for the same reason merging is: it puts work in front
   * of reviewers, and an agent should not decide when that happens.
   */
  async function markReady() {
    const pr = model.pr;
    if (!pr) return;
    setDetailsOpen(false);
    try {
      const next = await resource.runMutation("mark_ready", () =>
        client.markCodePrReady(workspaceId),
      );
      if (!next) return;
      resource.adopt(next);
      toast.success("Marked ready for review");
    } catch (err) {
      const message = friendlyErrorMessage(err, "Could not mark it ready");
      resource.setMutationError(message);
      toast.error(message);
    }
  }

  async function stopWatch() {
    setDetailsOpen(false);
    try {
      await resource.runMutation("stop_watch", async () => {
        await client.stopCodeWatch(workspaceId);
        await resource.refresh();
      });
      toast.success("Stopped watching");
    } catch (err) {
      const message = friendlyErrorMessage(err, "Could not stop the watch");
      resource.setMutationError(message);
      toast.error(message);
    }
  }

  async function run(action: WorkspaceWorkflowAction) {
    switch (action) {
      case "archive":
        onArchive?.();
        return;
      case "update_pr":
      case "follow_up_pr":
      case "sync_branch":
      case "resolve_divergence":
      case "resolve_local_conflicts": {
        setDetailsOpen(false);
        const prompt = workspaceActionPrompt(action, model.pr, baseRef);
        if (prompt && !runComposerPrompt(workspaceId, prompt))
          toast.error("Another agent action is already running");
        return;
      }
      case "open_source":
        setDetailsOpen(false);
        onOpenSourceControl();
        return;
      case "compose_pr":
        setDetailsOpen(false);
        if (!runComposerPrompt(workspaceId, composePrPrompt(baseRef))) {
          toast.error("Another agent action is already running");
        }
        return;
      case "open_pr": {
        setDetailsOpen(false);
        if (onOpenPr) {
          onOpenPr();
          return;
        }
        const url = model.pr?.url;
        if (url) void openExternal(url).catch(() => undefined);
        return;
      }
      case "push":
        try {
          const pushed = await resource.runMutation("push", async () => {
            await client.pushCodeWorkspace(workspaceId);
            await resource.refresh();
            return true;
          });
          if (!pushed) return;
          toast.success("Pushed");
        } catch (err) {
          const message =
            err instanceof HttpError && err.kind === "git_auth_failed"
              ? err.message
              : friendlyErrorMessage(err, "Could not push");
          resource.setMutationError(message);
          toast.error(message);
        }
        return;
      case "create_pr":
        try {
          const next = await resource.runMutation("create_pr", async () => {
            const created = await client.createCodePullRequest(workspaceId);
            resource.adopt(created);
            return created;
          });
          if (!next) return;
          const url = next.pr?.url;
          if (url && !(await openExternal(url).catch(() => false))) {
            toast.message("Open the pull request from workspace status.");
          }
          toast.success("Pull request created");
        } catch (err) {
          const message = friendlyErrorMessage(
            err,
            "Could not create a pull request",
          );
          resource.setMutationError(message);
          toast.error(message);
        }
        return;
      case "watch_and_fix":
        await runAgentAction("watch_and_fix");
        return;
      case "merge":
        await mergePr();
        return;
      case "mark_ready":
        await markReady();
        return;
      default:
        await runAgentAction(action);
    }
  }

  const disabled =
    busy !== null ||
    agentActionRunning ||
    attachingLogs ||
    resource.error !== null;
  async function refreshStatus() {
    try {
      await resource.refreshFromHost();
    } catch (error) {
      toast.error(
        friendlyErrorMessage(error, "Could not refresh workspace status"),
      );
    }
  }
  return (
    <>
      {confirmDialog}
      <div
        className="flex min-w-0 max-w-[min(48vw,32rem)] items-center overflow-hidden rounded-lg border border-border-subtle bg-page-background max-[1099px]:max-w-none max-[1099px]:flex-1"
        data-testid="workspace-workflow-control"
        data-stage={model.stage}
        data-tone={model.tone}
      >
        {model.pr && (
          <button
            type="button"
            className="h-control shrink-0 border-r border-border-subtle px-2 text-xs font-medium tabular-nums hover:bg-muted"
            data-testid="workspace-pr-chip"
            aria-label={`Open pull request #${model.pr.number}`}
            onClick={() => void run("open_pr")}
          >
            #{model.pr.number}
          </button>
        )}
        <Popover open={detailsOpen} onOpenChange={setDetailsOpen}>
          <PopoverTrigger asChild>
            <button
              type="button"
              className="flex h-control min-w-0 flex-1 items-center gap-1.5 px-2.5 text-xs hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring"
              aria-label={`Workspace status: ${workspaceStatusLabel(model)}`}
            >
              {resource.refreshing || busy === "refresh" ? (
                <Spinner className="size-3.5 shrink-0" />
              ) : (
                <GitBranch
                  className={cn(
                    "size-3.5 shrink-0",
                    STATUS_MARK[resource.error ? "warning" : model.tone],
                  )}
                  aria-hidden
                />
              )}
              <span
                className={cn(
                  "min-w-0 truncate",
                  STATUS_TEXT[resource.error ? "warning" : model.tone],
                )}
                aria-live="polite"
              >
                {resource.error
                  ? "Status unavailable"
                  : workspaceStatusLabel(model).replace(/^#\d+\s*·\s*/, "")}
              </span>
            </button>
          </PopoverTrigger>
          <PopoverContent
            align="end"
            sideOffset={7}
            className="w-[min(25rem,calc(100vw-24px))] p-0"
            data-testid="workspace-workflow-popover"
            role="dialog"
            aria-labelledby={popoverTitleId}
          >
            <div className="flex items-start gap-2 border-b border-border-subtle p-3">
              <div className="min-w-0 flex-1">
                <h2 id={popoverTitleId} className="text-md font-medium">
                  {detailsTitle}
                </h2>
                <p
                  className="mt-1 truncate font-mono text-xs text-muted-foreground"
                  title={branchName}
                >
                  {resource.data?.git?.branch ?? branchName}
                </p>
              </div>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label="Refresh workspace status"
                disabled={resource.refreshing || busy !== null}
                onClick={() => void refreshStatus()}
              >
                {resource.refreshing || busy === "refresh" ? (
                  <Spinner aria-hidden />
                ) : (
                  <RefreshCw aria-hidden />
                )}
              </Button>
            </div>
            <div className="p-3">
              <WorkspaceStatusDetails
                model={model}
                snapshot={resource.data}
                error={resource.mutationError ?? resource.error}
              />
            </div>
            <div className="flex flex-wrap gap-1 border-t border-border-subtle p-1.5">
              <Button variant="ghost" size="sm" onClick={onOpenSourceControl}>
                Source control
              </Button>
              {model.pr?.url && (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() =>
                    void openExternal(model.pr!.url!).catch(() => false)
                  }
                >
                  <ArrowUpRight aria-hidden />
                  Open on GitHub
                </Button>
              )}
              {watchActive && (
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busy !== null}
                  onClick={() => void stopWatch()}
                >
                  Stop background watch
                </Button>
              )}
            </div>
          </PopoverContent>
        </Popover>
        {watchActive && watch ? (
          <Button
            variant="ghost"
            size="sm"
            className="shrink-0 rounded-none border-l border-border-subtle"
            onClick={() =>
              onOpenWatchTask ? onOpenWatchTask() : void stopWatch()
            }
            disabled={busy !== null}
            data-testid="workspace-watch-control"
          >
            <CircleDotDashed aria-hidden />
            {watchStateLabel(watch.state)}
          </Button>
        ) : primary && primaryLabel ? (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-control shrink-0 rounded-none border-l border-border-subtle bg-foreground px-2.5 text-background hover:bg-foreground/90 hover:text-background"
            disabled={disabled || model.stage === "loading"}
            aria-busy={busy !== null || agentActionRunning || attachingLogs}
            onClick={() => void run(primary)}
            title={
              primary === "update_pr"
                ? "Commit and push your changes to this pull request."
                : primary === "watch_and_fix"
                  ? "Ask the agent in this conversation to monitor the PR and fix actionable failures."
                  : undefined
            }
          >
            {busy !== null || agentActionRunning || attachingLogs ? (
              <Spinner aria-hidden />
            ) : null}
            {attachingLogs ? "Reading logs…" : primaryLabel}
          </Button>
        ) : null}
        {secondaryActions.length > 0 && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="sm"
                className="h-control rounded-none border-l border-border-subtle px-1.5"
                aria-label="More workspace actions"
                disabled={busy !== null}
              >
                <ChevronDown aria-hidden />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {secondaryActions.map((action) => (
                <DropdownMenuItem
                  key={action}
                  disabled={
                    disabled && action !== "open_pr" && action !== "open_source"
                  }
                  onSelect={() => void run(action)}
                >
                  {workspaceWorkflowActionLabel(action, model.stage)}
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
    </>
  );
}

function watchStateLabel(state: CodeWatchState): string {
  switch (state) {
    case "watching":
      return "Watching";
    case "fixing":
      return "Fixing";
    case "blocked":
      return "Watch blocked";
    case "done":
      return "Watch done";
    case "stopped":
      return "Watch stopped";
    case "failed":
      return "Watch failed";
  }
}
