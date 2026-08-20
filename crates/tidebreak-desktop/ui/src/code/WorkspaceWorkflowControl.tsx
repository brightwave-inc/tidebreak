import { useId, useMemo, useState } from "react";
import {
  ChevronDown,
  CircleDotDashed,
  ExternalLink,
  GitBranch,
  GitPullRequest,
  RefreshCw,
} from "lucide-react";
import { toast } from "sonner";

import { HttpError, type ApiClient } from "../api/client";
import type {
  CodeWatchSnapshot,
  CodeWatchState,
  PullRequestDigest,
} from "../api/types";
import { Button } from "@/components/ui/button";
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
import { prWorkflowPrompt, type PrWorkflowAction } from "./prActions";
import { useCodeUiStore } from "./CodeUiStore";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";
import {
  checkSummary,
  workspaceWorkflowActionLabel,
  workspaceWorkflowModel,
  type WorkspaceWorkflowAction,
  type WorkspaceWorkflowTone,
} from "./workspaceWorkflow";

const TONE_CLASS: Record<WorkspaceWorkflowTone, string> = {
  neutral: "text-foreground-subtle",
  ready: "text-success",
  pending: "text-info-foreground",
  warning: "text-warning",
  critical: "text-critical",
};

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
  onOpenWatchTask,
}: {
  client: Pick<
    ApiClient,
    | "pushCodeWorkspace"
    | "createCodePullRequest"
    | "startCodeWatch"
    | "stopCodeWatch"
  >;
  workspaceId: string;
  branchName: string;
  baseRef?: string;
  fallbackPr?: PullRequestDigest;
  resource: CodeWorkspacePrResource;
  onOpenSourceControl: () => void;
  /** Open the watch task's transcript; the segment is a link to the fork. */
  onOpenWatchTask?: () => void;
}) {
  const [detailsOpen, setDetailsOpen] = useState(false);
  const popoverTitleId = useId();
  const runComposerPrompt = useCodeUiStore(
    (state) => state.runComposerPrompt,
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
  const primary = watchActive ? undefined : model.primary;
  const busy = resource.busy;
  const primaryLabel = primary
    ? workspaceWorkflowActionLabel(primary, model.stage)
    : null;
  const statusLabel = model.pr
    ? model.summary.replace(/^#\d+\s*·\s*/, "")
    : model.summary;
  const activeChecks =
    model.pr?.checks?.filter(
      (check) => check.bucket === "fail" || check.bucket === "pending",
    ) ?? [];
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

  function runAgentAction(action: Exclude<PrWorkflowAction, "watch_and_fix">) {
    const pr = model.pr;
    if (!pr) return;
    setDetailsOpen(false);
    runComposerPrompt(workspaceId, prWorkflowPrompt(action, pr));
  }

  async function startWatch() {
    setDetailsOpen(false);
    try {
      await resource.runMutation("watch", async () => {
        await client.startCodeWatch(workspaceId);
        await resource.refresh();
      });
      toast.success("Watching the pull request");
    } catch (err) {
      const message = friendlyErrorMessage(err, "Could not start the watch");
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
      case "open_source":
        setDetailsOpen(false);
        onOpenSourceControl();
        return;
      case "open_pr": {
        setDetailsOpen(false);
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
        await startWatch();
        return;
      default:
        runAgentAction(action);
    }
  }

  return (
    <div
      className="border-border-subtle bg-page-background/70 flex min-w-0 max-w-[min(40vw,26rem)] shrink items-center overflow-hidden rounded-lg border shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_6%,transparent)]"
      data-testid="workspace-workflow-control"
      data-stage={model.stage}
      data-tone={model.tone}
    >
      <Popover open={detailsOpen} onOpenChange={setDetailsOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            className={cn(
              "hover:bg-background focus-visible:ring-ring/25 flex h-8 min-w-0 flex-1 cursor-pointer items-center gap-1.5 bg-transparent px-2.5 text-xs font-medium transition-colors outline-none focus-visible:ring-3",
            )}
            aria-label={`Workspace status: ${model.summary}`}
          >
            {model.pr ? (
              <GitPullRequest
                className={cn("size-3.5 shrink-0", TONE_CLASS[model.tone])}
                aria-hidden
              />
            ) : (
              <GitBranch
                className={cn("size-3.5 shrink-0", TONE_CLASS[model.tone])}
                aria-hidden
              />
            )}
            {model.pr ? (
              <span className="shrink-0 font-semibold tabular-nums">
                #{model.pr.number}
              </span>
            ) : null}
            {model.pr ? (
              <span
                className="text-foreground-subtle max-[900px]:hidden"
                aria-hidden
              >
                ·
              </span>
            ) : null}
            <span
              className="text-foreground-subtle min-w-0 truncate tabular-nums max-[900px]:hidden"
              aria-live="polite"
            >
              {statusLabel}
            </span>
          </button>
        </PopoverTrigger>
        <PopoverContent
          align="end"
          sideOffset={7}
          className="w-[min(21rem,calc(100vw-24px))] overflow-hidden p-0"
          data-testid="workspace-workflow-popover"
          role="dialog"
          aria-labelledby={popoverTitleId}
        >
          <div className="flex items-start gap-2.5 border-b border-border-subtle px-3 py-3">
            <span
              className={cn(
                "mt-0.5 grid size-6 shrink-0 place-items-center rounded-md bg-muted/60",
                TONE_CLASS[model.tone],
              )}
            >
              {model.pr ? (
                <GitPullRequest className="size-3.5" aria-hidden />
              ) : (
                <GitBranch className="size-3.5" aria-hidden />
              )}
            </span>
            <div className="min-w-0 flex-1">
              <h2
                id={popoverTitleId}
                className="text-[13px] font-semibold leading-5"
              >
                {model.title}
              </h2>
              <p className="text-foreground-subtle mt-0.5 text-xs leading-4">
                {resource.mutationError ?? resource.error ?? model.detail}
              </p>
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label="Refresh workspace status"
              disabled={resource.refreshing || busy !== null}
              onClick={() => void resource.refresh()}
            >
              {resource.refreshing ? (
                <Spinner aria-hidden />
              ) : (
                <RefreshCw aria-hidden />
              )}
            </Button>
          </div>

          <dl className="divide-y divide-border-subtle px-3 text-xs">
            <div className="flex items-center gap-3 py-2.5">
              <dt className="text-foreground-subtle shrink-0">Branch</dt>
              <dd
                className="min-w-0 flex-1 truncate text-right font-mono text-[11px]"
                title={branchName}
              >
                {branchName}
              </dd>
            </div>
            {(model.pr?.base_branch ?? baseRef) ? (
              <div className="flex items-center gap-3 py-2.5">
                <dt className="text-foreground-subtle shrink-0">Base</dt>
                <dd className="min-w-0 flex-1 truncate text-right font-mono text-[11px]">
                  {model.pr?.base_branch ?? baseRef}
                </dd>
              </div>
            ) : null}
            {model.checks && model.checks.total > 0 ? (
              <div className="flex items-center gap-3 py-2.5">
                <dt className="text-foreground-subtle shrink-0">Checks</dt>
                <dd className="min-w-0 flex-1 truncate text-right tabular-nums">
                  {checkSummary(model.checks)}
                </dd>
              </div>
            ) : null}
            {watch ? (
              <div className="flex items-center gap-3 py-2.5">
                <dt className="text-foreground-subtle shrink-0">Watch</dt>
                <dd
                  className="min-w-0 flex-1 truncate text-right"
                  title={watch.detail}
                >
                  {watchStatusLabel(watch)}
                </dd>
              </div>
            ) : null}
          </dl>

          {activeChecks.length > 0 ? (
            <div className="border-t border-border-subtle px-3 py-2.5">
              <p className="text-foreground-subtle mb-1.5 text-[11px] font-medium">
                Active checks
              </p>
              <ul className="flex flex-col gap-1.5">
                {activeChecks.slice(0, 4).map((check, index) => (
                  <li
                    key={`${check.name}-${index}`}
                    className="flex min-w-0 items-center gap-2 text-xs"
                    title={check.detail}
                  >
                    <span
                      className={cn(
                        "size-1.5 shrink-0 rounded-full",
                        check.bucket === "fail"
                          ? "bg-critical"
                          : "bg-info-foreground",
                      )}
                      aria-hidden
                    />
                    <span className="min-w-0 flex-1 truncate">
                      {check.name}
                    </span>
                    <span
                      className={cn(
                        "shrink-0 text-[11px] font-medium",
                        check.bucket === "fail"
                          ? "text-critical"
                          : "text-info-foreground",
                      )}
                    >
                      {check.bucket === "fail" ? "Failed" : "Pending"}
                    </span>
                  </li>
                ))}
              </ul>
              {activeChecks.length > 4 ? (
                <p className="text-foreground-subtle mt-1.5 text-[11px]">
                  +{activeChecks.length - 4} more
                </p>
              ) : null}
            </div>
          ) : null}

          <div className="flex items-center gap-1 border-t border-border-subtle p-1.5">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="justify-start"
              onClick={() => void run("open_source")}
            >
              <GitBranch aria-hidden />
              Source control
            </Button>
            {watchActive && (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                disabled={busy !== null}
                onClick={() => void stopWatch()}
              >
                {busy === "stop_watch" ? "Stopping…" : "Stop watching"}
              </Button>
            )}
            {model.pr?.url ? (
              <Button asChild variant="ghost" size="sm" className="ml-auto">
                <a
                  href={model.pr.url}
                  onClick={(event) => {
                    event.preventDefault();
                    void run("open_pr");
                  }}
                >
                  <ExternalLink aria-hidden />
                  View PR
                </a>
              </Button>
            ) : null}
          </div>
        </PopoverContent>
      </Popover>

      {watchActive && watch ? (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="border-border-subtle min-w-0 rounded-none border-0 border-l bg-background px-2.5 hover:bg-muted/70"
          title={
            watch.detail
              ? `${watchStateLabel(watch.state)}: ${watch.detail}. Click to open the watch task.`
              : "A watch task is keeping this pull request moving. Click to open it."
          }
          disabled={busy !== null}
          aria-busy={busy === "stop_watch"}
          data-testid="workspace-watch-control"
          onClick={() => {
            if (onOpenWatchTask) {
              setDetailsOpen(false);
              onOpenWatchTask();
            } else {
              void stopWatch();
            }
          }}
        >
          {busy === "stop_watch" ? (
            <Spinner aria-hidden />
          ) : (
            <CircleDotDashed
              className={cn(
                watch.state === "blocked"
                  ? "text-warning"
                  : "text-info-foreground",
              )}
              aria-hidden
            />
          )}
          <span className="truncate max-[760px]:sr-only">
            {busy === "stop_watch"
              ? "Stopping…"
              : onOpenWatchTask
                ? `Watching in "Fix PR #${watch.pr_number}"`
                : watchStateLabel(watch.state)}
          </span>
        </Button>
      ) : primary === "open_pr" && primaryLabel && model.pr?.url ? (
        <Button
          asChild
          variant="ghost"
          size="sm"
          className="border-border-subtle min-w-0 rounded-none border-0 border-l bg-foreground px-2.5 text-background hover:bg-foreground/88 hover:text-background"
        >
          <a
            href={model.pr.url}
            aria-label={primaryLabel}
            onClick={(event) => {
              event.preventDefault();
              void run("open_pr");
            }}
          >
            <span className="truncate">{primaryLabel}</span>
          </a>
        </Button>
      ) : primary && primaryLabel ? (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="border-border-subtle min-w-0 rounded-none border-0 border-l bg-foreground px-2.5 text-background hover:bg-foreground/88 hover:text-background disabled:bg-muted disabled:text-muted-foreground"
          title={
            primary === "watch_and_fix"
              ? "Start an agent task that watches this pull request and fixes actionable failures."
              : primary === "mark_ready" ||
                  primary === "merge" ||
                  primary === "fix_errors" ||
                  primary === "address_feedback" ||
                  primary === "update_branch" ||
                  primary === "resolve_conflicts"
                ? `Start an agent task to ${primaryLabel.toLowerCase()}.`
                : undefined
          }
          disabled={
            busy !== null || agentActionRunning || model.stage === "loading"
          }
          aria-busy={busy === primary || agentActionRunning}
          onClick={() => void run(primary)}
        >
          {busy === primary || agentActionRunning ? (
            <Spinner aria-hidden />
          ) : primary === "watch_and_fix" ? (
            <CircleDotDashed aria-hidden />
          ) : null}
          <span
            className={cn(
              "truncate",
              primary === "watch_and_fix" && "max-[760px]:sr-only",
            )}
          >
            {busy === "push" && primary === "push"
              ? "Pushing…"
              : busy === "create_pr" && primary === "create_pr"
                ? "Creating…"
                : primaryLabel}
          </span>
        </Button>
      ) : null}

      {secondaryActions.length > 0 ? (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="border-border-subtle rounded-none border-0 border-l bg-transparent px-1.5 hover:bg-background"
              aria-label="More workspace actions"
              disabled={busy !== null || agentActionRunning}
            >
              <ChevronDown aria-hidden />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-44">
            {secondaryActions.map((action) => (
              <DropdownMenuItem key={action} onSelect={() => void run(action)}>
                {workspaceWorkflowActionLabel(action, model.stage)}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      ) : null}
    </div>
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

function watchStatusLabel(watch: CodeWatchSnapshot): string {
  const base = watch.detail ?? watchStateLabel(watch.state);
  return watch.cycles > 0
    ? `${base} · ${watch.cycles} fix ${watch.cycles === 1 ? "turn" : "turns"}`
    : base;
}
