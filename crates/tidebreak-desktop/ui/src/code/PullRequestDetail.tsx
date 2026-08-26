import { useEffect, useMemo, useRef, useState } from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import {
  Bot,
  Check,
  CircleAlert,
  CircleDot,
  ExternalLink,
  GitBranch,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  GitPullRequestDraft,
  LoaderCircle,
  MessageSquare,
  MoreHorizontal,
  Play,
  RefreshCw,
  ShieldAlert,
  X,
} from "lucide-react";
import { formatDistanceToNowStrict } from "date-fns";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type {
  CodeDeliveryActionResult,
  CodeDeliveryCheck,
  CodeDeliveryPullRequestAction,
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestFile,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryStackMember,
} from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogOverlay,
  DialogPortal,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { MessageMarkdown } from "@/MessageMarkdown";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { openInBrowser } from "@/openInBrowser";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import { codeDeliveryRepositoryTarget } from "./CodeDeliveryStore";
import { fetchFixErrorsLogs } from "./checkLogs";
import { MiddleTruncate } from "./MiddleTruncate";
import {
  deliveryPullRequestDigest,
  prAgentQuickActions,
  prDirectMergeAction,
  prFreshAgentPrompt,
  prWorkflowPrompt,
  type PrPromptAction,
} from "./prActions";
import { GithubAvatar } from "./GithubAvatar";
import { PrCommentCard } from "./PrCommentCard";
import {
  expandGithubEmojiShortcodes,
  fileStatusLabel,
  fileStatusTone,
  orderPullRequestComments,
  type PullRequestCommentOrder,
} from "./pullRequestPresentation";
import {
  PULL_REQUEST_LIFECYCLE_LABEL,
  PULL_REQUEST_LIFECYCLE_TONE,
  STATUS_TONE_BADGE_VARIANT,
  checkCounts,
  mergeBlockedReasons,
  pullRequestLifecycle,
  pullRequestSettledAt,
  type PullRequestLifecycle,
} from "./prState";
import { STATUS_MARK, STATUS_TEXT } from "./statusTone";

type MergeMethod = "squash" | "merge" | "rebase";
type DetailTab = "conversation" | "files" | "checks";

/**
 * The frame both delivery detail surfaces share: a large sheet floated over
 * the list rather than a column squeezed beside it. The list keeps its full
 * width, the detail gets room for a real diff, and closing is the ordinary
 * dialog vocabulary — Escape, the overlay, or the header's X.
 */
export function DetailSheet({
  label,
  onClose,
  children,
}: {
  /** Accessible name for the sheet; the visible header carries the title. */
  label: string;
  onClose: () => void;
  children: React.ReactNode;
}) {
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogPortal>
        <DialogOverlay className="bg-black/40" />
        <DialogPrimitive.Content
          aria-describedby={undefined}
          className="fixed top-1/2 left-1/2 z-50 flex h-[min(52rem,calc(100vh-3rem))] w-[min(66rem,calc(100vw-2.5rem))] translate-x-[-50%] translate-y-[-50%] flex-col overflow-hidden rounded-xl border border-border-subtle bg-background shadow-lg outline-none duration-200 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95"
        >
          <DialogTitle className="sr-only">{label}</DialogTitle>
          {children}
        </DialogPrimitive.Content>
      </DialogPortal>
    </Dialog>
  );
}

/**
 * The pull request, as close to its GitHub page as a desktop sheet gets.
 *
 * The point is that a reader finishes here. Everything the GitHub page leads
 * with — lifecycle, who merged it and when, the branch pair, the diffstat,
 * labels and reviewers, the description as real Markdown, the conversation,
 * every check, every changed file — is on this surface, and the actions that
 * page offers (ready, merge, auto-merge, rerun, close, reopen, comment) run
 * from it. "Open on GitHub" stays, but as an escape hatch rather than the
 * only way to see what happened. Beyond the page itself, the sheet links the
 * pull request to the Tidebreak workspaces that carry it and can put an
 * agent — linked or fresh — onto its remaining chores.
 */
export function PullRequestDetailSheet({
  client,
  summary,
  initialDetail,
  hasMergeQueue = false,
  onClose,
  onChanged,
  onOpenWorkspace,
}: {
  client: Pick<
    ApiClient,
    | "getCodeDeliveryPullRequestDetail"
    | "runCodeDeliveryPullRequestAction"
    | "createCodeWorkspace"
    | "writeCodeCheckLogs"
  >;
  summary: CodeDeliveryPullRequestSummary;
  /** True when this repository already uses a merge queue. */
  hasMergeQueue?: boolean;
  initialDetail?: CodeDeliveryPullRequestDetail;
  onClose: () => void;
  onChanged: () => void;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const [detail, setDetail] = useState<CodeDeliveryPullRequestDetail | null>(
    initialDetail ?? null,
  );
  const [loading, setLoading] = useState(!initialDetail);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [tab, setTab] = useState<DetailTab>("conversation");
  const [mergeMethod, setMergeMethod] = useState<MergeMethod>("squash");
  const [commentOrder, setCommentOrder] =
    useState<PullRequestCommentOrder>("newest");
  const [draftComment, setDraftComment] = useState("");
  // Inline rather than a dialog: the sheet is already a modal, and a second
  // Radix modal stacked on it shares the dismiss layer and the body
  // pointer-events lock — the class of bug #2537 hit with a dialog over a
  // dialog. The confirmation renders inside the actions card instead.
  const [confirmingAdminMerge, setConfirmingAdminMerge] = useState(false);
  const generation = useRef(0);
  const activeTarget = useRef(summary.id);
  const mounted = useRef(true);
  activeTarget.current = summary.id;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const targetIsActive = (targetId: string) =>
    mounted.current && activeTarget.current === targetId;

  const load = async () => {
    const targetId = summary.id;
    const token = ++generation.current;
    setLoading(true);
    setError(null);
    try {
      const next = await client.getCodeDeliveryPullRequestDetail({
        repository: codeDeliveryRepositoryTarget(summary.repository),
        number: summary.number,
      });
      if (token === generation.current && targetIsActive(targetId)) {
        setDetail(next);
      }
    } catch (caught) {
      if (token === generation.current && targetIsActive(targetId)) {
        setError(
          friendlyErrorMessage(caught, "Could not load this pull request."),
        );
      }
    } finally {
      if (token === generation.current && targetIsActive(targetId)) {
        setLoading(false);
      }
    }
  };

  useEffect(() => {
    setTab("conversation");
    setCommentOrder("newest");
    setDraftComment("");
    setConfirmingAdminMerge(false);
    if (initialDetail?.summary.id === summary.id) {
      setDetail(initialDetail);
      setLoading(false);
    } else {
      setDetail(null);
      void load();
    }
    return () => {
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, initialDetail, summary.id]);

  const runAction = async (
    name: string,
    action: CodeDeliveryPullRequestAction,
  ) => {
    if (busy) return;
    const targetId = summary.id;
    setBusy(name);
    try {
      const result = await client.runCodeDeliveryPullRequestAction({
        target: {
          repository: codeDeliveryRepositoryTarget(summary.repository),
          number: summary.number,
        },
        action,
      });
      if (!targetIsActive(targetId)) return;
      if (result.success) {
        toast.success(result.message);
      } else {
        const description = rerunOutcomeDescription(result, current.checks);
        toast.warning(
          result.message,
          description ? { description } : undefined,
        );
      }
      if (action.type === "comment") setDraftComment("");
      onChanged();
      await load();
    } catch (caught) {
      if (!targetIsActive(targetId)) return;
      toast.error(
        friendlyErrorMessage(caught, "The pull request action failed."),
      );
    } finally {
      if (targetIsActive(targetId)) setBusy(null);
    }
  };

  // Prefer the freshly loaded summary: an action just changed it, and the row
  // behind this sheet may still be a poll behind.
  const current = detail?.summary ?? summary;
  const lifecycle = pullRequestLifecycle(current);
  const counts = checkCounts(current);
  const workflowRunIds = useMemo(
    () => [
      ...new Set(
        current.checks
          .filter((check) => check.bucket === "fail" && check.workflow_run_id)
          .map((check) => check.workflow_run_id!),
      ),
    ],
    [current.checks],
  );

  const mergeStack = async (members: readonly CodeDeliveryStackMember[]) => {
    if (busy) return;
    const targetId = summary.id;
    setBusy("merge-stack");
    try {
      for (const member of members) {
        // Merging a lower layer rebases the ones above, so every hop reads a
        // fresh head rather than trusting the SHA this sheet loaded — the
        // expected-head guard would refuse a stale one.
        const fresh = await client.getCodeDeliveryPullRequestDetail({
          repository: codeDeliveryRepositoryTarget(summary.repository),
          number: member.number,
        });
        const head = fresh.summary.head_sha;
        if (fresh.summary.state !== "open" || !head) continue;
        const result = await client.runCodeDeliveryPullRequestAction({
          target: {
            repository: codeDeliveryRepositoryTarget(summary.repository),
            number: member.number,
          },
          action: {
            type: "merge",
            method: mergeMethod,
            auto: hasMergeQueue,
            admin: false,
            expected_head_sha: head,
          },
        });
        if (!result.success) {
          toast.warning(
            `Stack merge stopped at #${member.number}: ${result.message}`,
          );
          return;
        }
      }
      if (targetIsActive(targetId)) {
        toast.success("Stack merged.");
      }
    } catch (caught) {
      if (!targetIsActive(targetId)) return;
      toast.error(friendlyErrorMessage(caught, "The stack merge failed."));
    } finally {
      if (targetIsActive(targetId)) {
        setBusy(null);
        onChanged();
        await load();
      }
    }
  };

  const adminMerge = async () => {
    if (!current.head_sha) return;
    setConfirmingAdminMerge(false);
    await runAction("admin-merge", {
      type: "merge",
      method: mergeMethod,
      auto: false,
      admin: true,
      expected_head_sha: current.head_sha,
    });
  };

  return (
    <DetailSheet
      label={`Pull request #${summary.number}: ${summary.title}`}
      onClose={onClose}
    >
      <PrDetailHeader
        summary={current}
        lifecycle={lifecycle}
        detail={detail}
        onClose={onClose}
      />

      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border-subtle px-5 py-2.5">
        <Button
          type="button"
          size="xs"
          variant="outline"
          onClick={() => void openInBrowser(current.url)}
        >
          <ExternalLink />
          Open on GitHub
        </Button>
        {current.workspace_links.map((workspace) => (
          <Button
            key={workspace.workspace_id}
            type="button"
            size="xs"
            variant="outline"
            title={`Open the linked workspace on ${workspace.branch_name}`}
            onClick={() => onOpenWorkspace(workspace.workspace_id)}
          >
            <GitBranch />
            Open {workspace.title}
          </Button>
        ))}
        <PrAgentMenu
          client={client}
          summary={current}
          onOpenWorkspace={onOpenWorkspace}
        />
        <Button
          type="button"
          size="xs"
          variant="ghost"
          className="ml-auto"
          disabled={loading}
          onClick={() => void load()}
        >
          {loading ? <LoaderCircle className="animate-spin" /> : <RefreshCw />}
          Refresh
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {loading && !detail ? (
          <div className="p-5">
            <DetailSkeleton />
          </div>
        ) : error ? (
          <InlineDetailError message={error} onRetry={() => void load()} />
        ) : detail ? (
          <>
            <DetailErrors errors={detail.errors} />
            <Tabs
              value={tab}
              onValueChange={(value) => setTab(value as DetailTab)}
            >
              <TabsList className="sticky top-0 z-10 w-full justify-start rounded-none border-b border-border-subtle bg-background/95 px-5 backdrop-blur">
                <TabsTrigger value="conversation">
                  <MessageSquare />
                  Conversation
                  {detail.comments.length > 0 && (
                    <span className="text-muted-foreground tabular-nums">
                      {detail.comments.length}
                    </span>
                  )}
                </TabsTrigger>
                <TabsTrigger value="files">
                  Files
                  <span className="text-muted-foreground tabular-nums">
                    {detail.changed_files}
                  </span>
                </TabsTrigger>
                <TabsTrigger value="checks">
                  Checks
                  {counts.total > 0 && (
                    <span
                      className={cn(
                        "tabular-nums",
                        counts.failing > 0
                          ? STATUS_TEXT.critical
                          : counts.pending > 0
                            ? STATUS_TEXT.pending
                            : STATUS_TEXT.ready,
                      )}
                    >
                      {counts.passing}/{counts.total}
                    </span>
                  )}
                </TabsTrigger>
              </TabsList>

              <TabsContent
                value="conversation"
                className="mt-0 flex flex-col gap-5 p-5"
              >
                <PrActions
                  detail={detail}
                  summary={current}
                  hasMergeQueue={hasMergeQueue}
                  busy={busy}
                  mergeMethod={mergeMethod}
                  onMergeMethodChange={setMergeMethod}
                  workflowRunIds={workflowRunIds}
                  onRun={(name, action) => void runAction(name, action)}
                  confirmingAdminMerge={confirmingAdminMerge}
                  onAdminMergeRequest={() => setConfirmingAdminMerge(true)}
                  onAdminMergeCancel={() => setConfirmingAdminMerge(false)}
                  onAdminMergeConfirm={() => void adminMerge()}
                  onMergeStack={(members) => void mergeStack(members)}
                />
                <PrDescription body={detail.body} />
                <PrConversation
                  detail={detail}
                  busy={busy}
                  order={commentOrder}
                  onOrderChange={setCommentOrder}
                  draft={draftComment}
                  onDraftChange={setDraftComment}
                  onComment={() =>
                    void runAction("comment", {
                      type: "comment",
                      body: draftComment,
                    })
                  }
                />
              </TabsContent>

              <TabsContent value="files" className="mt-0 p-5">
                <PrFiles detail={detail} />
              </TabsContent>

              <TabsContent value="checks" className="mt-0 p-5">
                <PrChecks checks={current.checks} />
              </TabsContent>
            </Tabs>
          </>
        ) : null}
      </div>
    </DetailSheet>
  );
}

function rerunOutcomeDescription(
  result: CodeDeliveryActionResult,
  checks: CodeDeliveryCheck[],
): string | undefined {
  if (!result.rerun_outcomes?.length) return undefined;
  const checkNames = new Map<number, Set<string>>();
  for (const check of checks) {
    if (!check.workflow_run_id || check.bucket !== "fail") continue;
    const names = checkNames.get(check.workflow_run_id) ?? new Set<string>();
    names.add(check.name);
    checkNames.set(check.workflow_run_id, names);
  }
  const label = (workflowRunId: number) => {
    const names = [...(checkNames.get(workflowRunId) ?? [])];
    return names.length > 0
      ? `${names.join(", ")} (run ${workflowRunId})`
      : `Run ${workflowRunId}`;
  };
  const queued = result.rerun_outcomes
    .filter((outcome) => outcome.success)
    .map((outcome) => label(outcome.workflow_run_id));
  const failed = result.rerun_outcomes
    .filter((outcome) => !outcome.success)
    .map(
      (outcome) =>
        `${label(outcome.workflow_run_id)}: ${outcome.error ?? "Unknown error"}`,
    );
  return [
    queued.length > 0 ? `Queued: ${queued.join(", ")}.` : null,
    failed.length > 0 ? `Failed: ${failed.join("; ")}.` : null,
  ]
    .filter((line): line is string => Boolean(line))
    .join("\n");
}

function DetailErrors({
  errors,
}: {
  errors: CodeDeliveryPullRequestDetail["errors"];
}) {
  if (errors.length === 0) return null;
  return (
    <div
      role="alert"
      className="m-5 mb-0 rounded-lg border border-warning/30 bg-warning/5 px-3 py-2.5 text-xs"
    >
      <p className="font-medium text-warning">Some details could not load.</p>
      <ul className="mt-1 list-disc space-y-1 pl-4 text-muted-foreground">
        {errors.map((error, index) => (
          <li key={`${error.kind}:${error.message}:${index}`}>
            {error.message}
          </li>
        ))}
      </ul>
    </div>
  );
}

function PrDetailHeader({
  summary,
  lifecycle,
  detail,
  onClose,
}: {
  summary: CodeDeliveryPullRequestSummary;
  lifecycle: PullRequestLifecycle;
  detail: CodeDeliveryPullRequestDetail | null;
  onClose: () => void;
}) {
  const settledAt = pullRequestSettledAt(summary);
  return (
    <div className="shrink-0 border-b border-border-subtle px-5 py-3">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
            <span className="truncate">
              {summary.repository.name_with_owner}
            </span>
            <span className="tabular-nums">#{summary.number}</span>
          </div>
          <h2 className="mt-1 text-base font-semibold leading-snug">
            {summary.title}
          </h2>
        </div>
        <Button type="button" size="icon-xs" variant="ghost" onClick={onClose}>
          <X />
          <span className="sr-only">Close pull request details</span>
        </Button>
      </div>

      <div className="mt-2.5 flex flex-wrap items-center gap-2">
        <Badge
          variant={
            STATUS_TONE_BADGE_VARIANT[PULL_REQUEST_LIFECYCLE_TONE[lifecycle]]
          }
          size="sm"
        >
          <PrLifecycleIcon lifecycle={lifecycle} className="size-3" />
          {PULL_REQUEST_LIFECYCLE_LABEL[lifecycle]}
        </Badge>
        {summary.auto_merge_enabled && lifecycle === "open" && (
          <Badge variant="info" size="sm">
            <GitMerge className="size-3" />
            Auto-merge
          </Badge>
        )}
        {detail && (
          <span className="font-mono text-xs tabular-nums">
            <span className={STATUS_TEXT.ready}>+{detail.additions}</span>{" "}
            <span className={STATUS_TEXT.critical}>−{detail.deletions}</span>
          </span>
        )}
      </div>

      {detail?.stack && detail.stack.length > 1 && (
        <StackMap
          stack={detail.stack}
          currentNumber={summary.number}
          url={summary.url}
        />
      )}

      <p className="mt-2 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs text-muted-foreground">
        <GithubAvatar
          login={summary.author}
          url={summary.author_avatar_url}
          className="size-4"
        />
        <span className="font-medium text-foreground">
          {summary.author ?? "Unknown"}
        </span>
        <span>opened {relativeTime(summary.created_at)}</span>
        {settledAt && (
          <>
            <span aria-hidden>·</span>
            <span>
              {lifecycle === "merged" ? "merged" : "closed"}{" "}
              {relativeTime(settledAt)}
              {detail?.merged_by ? ` by ${detail.merged_by}` : ""}
            </span>
          </>
        )}
      </p>

      <p className="mt-1.5 flex min-w-0 items-center gap-1.5 font-mono text-xs text-muted-foreground">
        <GitBranch className="size-3 shrink-0" />
        <MiddleTruncate className="min-w-0" text={summary.base_branch} />
        <span aria-hidden>←</span>
        <MiddleTruncate className="min-w-0" text={summary.head_branch} />
      </p>

      {(summary.labels.length > 0 ||
        (detail?.assignees.length ?? 0) > 0 ||
        (detail?.requested_reviewers.length ?? 0) > 0) && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          {summary.labels.map((label) => (
            <Badge key={label} variant="secondary" size="sm">
              {label}
            </Badge>
          ))}
          {detail?.assignees.map((login) => (
            <span
              key={`assignee:${login}`}
              className="flex items-center gap-1 text-xs text-muted-foreground"
            >
              <GithubAvatar login={login} className="size-4" />
              {login}
            </span>
          ))}
          {detail?.requested_reviewers.map((login) => (
            <span
              key={`reviewer:${login}`}
              className="flex items-center gap-1 text-xs text-muted-foreground"
              title={`Review requested from ${login}`}
            >
              <GithubAvatar login={login} className="size-4" />
              {login}
              <span className="text-2xs">(review requested)</span>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * Put an agent onto this pull request's remaining chores.
 *
 * The menu offers only what the pull request actually has — conflicts,
 * failing checks, requested changes, a stale base — and says where the agent
 * runs before anything starts. A linked active workspace is the natural
 * target because its branch is the pull request's own; without one, a fresh
 * workspace is cut from the head branch and the prompt tells the agent how
 * to push back to it.
 */
function PrAgentMenu({
  client,
  summary,
  onOpenWorkspace,
}: {
  client: Pick<ApiClient, "createCodeWorkspace" | "writeCodeCheckLogs">;
  summary: CodeDeliveryPullRequestSummary;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const [starting, setStarting] = useState(false);
  const digest = useMemo(() => deliveryPullRequestDigest(summary), [summary]);
  const actions = useMemo(() => prAgentQuickActions(digest), [digest]);
  const link =
    summary.workspace_links.find(
      (candidate) => candidate.exact && candidate.status === "active",
    ) ??
    summary.workspace_links.find((candidate) => candidate.status === "active");
  const repoId = summary.repository.tidebreak_repo_id;
  if (actions.length === 0 || (!link && !repoId)) return null;

  const run = async (action: PrPromptAction) => {
    if (useCodeUiStore.getState().composerActionScope !== null) {
      toast.error("Another agent action is already running");
      return;
    }
    setStarting(true);
    try {
      if (link) {
        const logs =
          action === "fix_errors"
            ? await fetchFixErrorsLogs(client, link.workspace_id)
            : [];
        if (
          !useCodeUiStore
            .getState()
            .runComposerPrompt(
              link.workspace_id,
              prWorkflowPrompt(action, digest, logs),
            )
        ) {
          toast.error("Another agent action is already running");
          return;
        }
        onOpenWorkspace(link.workspace_id);
        return;
      }
      // No linked workspace: cut a fresh one from the pull request's head.
      // Log download is skipped on purpose — it reads the workspace's own
      // pull request digest, which a just-created workspace does not have;
      // the prompt's fallback already tells the agent to read CI itself.
      const workspace = await client.createCodeWorkspace({
        repo_id: repoId!,
        title: freshAgentWorkspaceTitle(summary),
        base_ref: summary.head_branch,
      });
      useCodeCatalogStore.getState().upsertWorkspace(workspace);
      if (
        !useCodeUiStore
          .getState()
          .runComposerPrompt(workspace.id, prFreshAgentPrompt(action, digest))
      ) {
        toast.error("Another agent action is already running");
        return;
      }
      onOpenWorkspace(workspace.id);
    } catch (caught) {
      toast.error(
        friendlyErrorMessage(
          caught,
          "Could not start an agent on this pull request.",
        ),
      );
    } finally {
      setStarting(false);
    }
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button type="button" size="xs" variant="outline" disabled={starting}>
          {starting ? <LoaderCircle className="animate-spin" /> : <Bot />}
          Fix with an agent
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start">
        <div className="max-w-64 px-2 py-1.5 text-xs text-muted-foreground">
          {link
            ? `Runs in ${link.title}, the linked workspace.`
            : `Starts a fresh workspace on ${summary.repository.name_with_owner}.`}
        </div>
        <DropdownMenuSeparator />
        {actions.map((item) => (
          <DropdownMenuItem
            key={item.action}
            onSelect={() => void run(item.action)}
          >
            {item.label}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Readable in the rail, and short enough to slug into a branch name. */
function freshAgentWorkspaceTitle(
  summary: CodeDeliveryPullRequestSummary,
): string {
  const base = `PR #${summary.number} ${summary.title}`.trim();
  return base.length > 60 ? `${base.slice(0, 59).trimEnd()}…` : base;
}

function PrActions({
  detail,
  summary,
  hasMergeQueue,
  busy,
  mergeMethod,
  onMergeMethodChange,
  workflowRunIds,
  onRun,
  confirmingAdminMerge,
  onAdminMergeRequest,
  onAdminMergeCancel,
  onAdminMergeConfirm,
  onMergeStack,
}: {
  detail: CodeDeliveryPullRequestDetail;
  summary: CodeDeliveryPullRequestSummary;
  hasMergeQueue: boolean;
  busy: string | null;
  mergeMethod: MergeMethod;
  onMergeMethodChange: (method: MergeMethod) => void;
  workflowRunIds: number[];
  onRun: (name: string, action: CodeDeliveryPullRequestAction) => void;
  confirmingAdminMerge: boolean;
  onAdminMergeRequest: () => void;
  onAdminMergeCancel: () => void;
  onAdminMergeConfirm: () => void;
  onMergeStack: (members: readonly CodeDeliveryStackMember[]) => void;
}) {
  const [confirmingStackMerge, setConfirmingStackMerge] = useState(false);
  const blockers = mergeBlockedReasons(summary);
  const mergeAction =
    detail.can_merge && summary.head_sha
      ? prDirectMergeAction(deliveryPullRequestDigest(summary), {
          hasMergeQueue,
        })
      : null;
  const canRerun = detail.can_rerun_failed && workflowRunIds.length > 0;
  const canAdminMerge = detail.can_merge && Boolean(summary.head_sha);
  // The stack offer is GitHub's own: merging lands the bottom run of
  // non-draft layers, in order. Merged layers below the run are skipped, a
  // draft layer stops it — GitHub lands everything below the latest ready
  // pull request and leaves the drafts above open. Two layers is the
  // smallest stack worth confirming, and while the offer is on the table it
  // replaces the single-layer merge: merging one layer of a live stack alone
  // is the half-measure the stack exists to avoid.
  const mergeableStackLayers: CodeDeliveryStackMember[] = [];
  for (const member of detail.stack ?? []) {
    if (member.state !== "open") continue;
    if (member.draft) break;
    mergeableStackLayers.push(member);
  }
  const canMergeStack =
    detail.can_merge &&
    Boolean(summary.head_sha) &&
    mergeableStackLayers.length >= 2;
  const showSingleMerge = !canMergeStack && mergeAction !== null;
  const anyAction =
    detail.can_mark_ready ||
    mergeAction !== null ||
    canMergeStack ||
    canAdminMerge ||
    detail.can_close ||
    detail.can_reopen ||
    canRerun;
  if (!anyAction) return null;
  return (
    <section className="rounded-lg border border-border-subtle bg-muted/20 p-3">
      <div className="flex flex-wrap items-center gap-2">
        {detail.can_mark_ready && (
          <Button
            type="button"
            size="sm"
            disabled={Boolean(busy)}
            onClick={() => onRun("ready", { type: "mark_ready" })}
          >
            {busy === "ready" && <LoaderCircle className="animate-spin" />}
            Mark ready
          </Button>
        )}
        {showSingleMerge && summary.head_sha && (
          <>
            <Select
              value={mergeMethod}
              onValueChange={(value) =>
                onMergeMethodChange(value as MergeMethod)
              }
            >
              <SelectTrigger size="sm" className="w-28">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="squash">Squash</SelectItem>
                <SelectItem value="merge">Merge</SelectItem>
                <SelectItem value="rebase">Rebase</SelectItem>
              </SelectContent>
            </Select>
            <Button
              type="button"
              size="sm"
              variant={mergeAction.kind === "merge" ? "default" : "outline"}
              disabled={Boolean(busy)}
              onClick={() =>
                onRun(mergeAction.kind, {
                  type: "merge",
                  method: mergeMethod,
                  auto: mergeAction.auto,
                  admin: false,
                  expected_head_sha: summary.head_sha!,
                })
              }
            >
              {busy === mergeAction.kind && (
                <LoaderCircle className="animate-spin" />
              )}
              {mergeAction.kind === "merge" ? <GitMerge /> : null}
              {mergeAction.label}
            </Button>
          </>
        )}

        {canMergeStack && (
          <>
            <Select
              value={mergeMethod}
              onValueChange={(value) =>
                onMergeMethodChange(value as MergeMethod)
              }
            >
              <SelectTrigger size="sm" className="w-28">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="squash">Squash</SelectItem>
                <SelectItem value="merge">Merge</SelectItem>
                <SelectItem value="rebase">Rebase</SelectItem>
              </SelectContent>
            </Select>
            <Button
              type="button"
              size="sm"
              variant="default"
              disabled={Boolean(busy)}
              onClick={() => setConfirmingStackMerge(true)}
            >
              {busy === "merge-stack" && (
                <LoaderCircle className="animate-spin" />
              )}
              <GitMerge />
              Merge stack ({mergeableStackLayers.length} layers)
            </Button>
            {confirmingStackMerge && (
              <div className="mt-2.5 flex w-full flex-col gap-2 rounded-md border border-border-subtle bg-background p-2.5">
                <p className="text-muted-foreground text-xs">
                  Lands {mergeableStackLayers.length} layers bottom to top:
                  {mergeableStackLayers.map((member) => ` #${member.number}`)}.
                  Each layer merges with{" "}
                  {hasMergeQueue
                    ? "the merge queue"
                    : `a direct ${mergeMethod} merge`}
                  . The chain stops at the first layer that cannot merge, and
                  draft layers above it stay open.
                </p>
                <div className="flex gap-2">
                  <Button
                    type="button"
                    size="xs"
                    disabled={Boolean(busy)}
                    onClick={() => {
                      setConfirmingStackMerge(false);
                      onMergeStack(mergeableStackLayers);
                    }}
                  >
                    Merge stack
                  </Button>
                  <Button
                    type="button"
                    size="xs"
                    variant="ghost"
                    onClick={() => setConfirmingStackMerge(false)}
                  >
                    Cancel
                  </Button>
                </div>
              </div>
            )}
          </>
        )}
        {canRerun && (
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={Boolean(busy)}
            onClick={() =>
              onRun("rerun", {
                type: "rerun_failed",
                workflow_run_ids: workflowRunIds,
              })
            }
          >
            {busy === "rerun" ? (
              <LoaderCircle className="animate-spin" />
            ) : (
              <RefreshCw />
            )}
            Rerun failed
          </Button>
        )}
        {detail.can_reopen && (
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={Boolean(busy)}
            onClick={() => onRun("reopen", { type: "reopen" })}
          >
            {busy === "reopen" && <LoaderCircle className="animate-spin" />}
            Reopen
          </Button>
        )}
        {(detail.can_close || canAdminMerge) && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                disabled={Boolean(busy)}
                aria-label="More pull request actions"
              >
                {busy === "close" || busy === "admin-merge" ? (
                  <LoaderCircle className="animate-spin" />
                ) : (
                  <MoreHorizontal />
                )}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {canAdminMerge && (
                <DropdownMenuItem onSelect={onAdminMergeRequest}>
                  <ShieldAlert />
                  Admin merge (bypass protections)…
                </DropdownMenuItem>
              )}
              {detail.can_close && (
                <DropdownMenuItem
                  variant="destructive"
                  onSelect={() => onRun("close", { type: "close" })}
                >
                  <GitPullRequestClosed />
                  Close without merging
                </DropdownMenuItem>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
      {confirmingAdminMerge && canAdminMerge && (
        <div className="mt-2.5 flex flex-col gap-2 rounded-md border border-critical-border bg-critical-background/40 p-2.5">
          <p className="flex items-start gap-1.5 text-xs text-critical-foreground-muted">
            <ShieldAlert className="mt-0.5 size-3.5 shrink-0" />
            Admin merge lands this pull request now and skips any reviews and
            checks the branch still requires. GitHub records the bypass under
            your account.
          </p>
          <div className="flex items-center gap-2">
            <Button
              type="button"
              size="xs"
              variant="destructive"
              disabled={Boolean(busy)}
              onClick={onAdminMergeConfirm}
            >
              {busy === "admin-merge" && (
                <LoaderCircle className="animate-spin" />
              )}
              Admin merge
            </Button>
            <Button
              type="button"
              size="xs"
              variant="ghost"
              disabled={Boolean(busy)}
              onClick={onAdminMergeCancel}
            >
              Cancel
            </Button>
          </div>
        </div>
      )}
      {blockers.length > 0 && detail.can_merge && mergeAction === null && (
        <ul className="mt-2.5 flex flex-col gap-1 border-t border-border-subtle pt-2.5 text-xs text-warning-foreground">
          {blockers.map((reason) => (
            <li key={reason} className="flex items-start gap-1.5">
              <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
              {reason}
            </li>
          ))}
        </ul>
      )}
      {summary.workspace_links.length === 0 && (
        <p className="mt-2.5 border-t border-border-subtle pt-2.5 text-xs text-muted-foreground">
          Not linked to a Tidebreak workspace. These GitHub actions still work;
          code changes need a workspace.
        </p>
      )}
    </section>
  );
}

function PrDescription({ body }: { body: string }) {
  const trimmed = body.trim();
  return (
    <section>
      <h3 className="text-sm font-medium">Description</h3>
      {trimmed ? (
        <div className="review-comment-markdown mt-2 text-md leading-5">
          <MessageMarkdown>
            {expandGithubEmojiShortcodes(trimmed)}
          </MessageMarkdown>
        </div>
      ) : (
        <p className="mt-2 text-xs text-muted-foreground">
          No description provided.
        </p>
      )}
    </section>
  );
}

function PrConversation({
  detail,
  busy,
  order,
  onOrderChange,
  draft,
  onDraftChange,
  onComment,
}: {
  detail: CodeDeliveryPullRequestDetail;
  busy: string | null;
  order: PullRequestCommentOrder;
  onOrderChange: (order: PullRequestCommentOrder) => void;
  draft: string;
  onDraftChange: (value: string) => void;
  onComment: () => void;
}) {
  const ordered = useMemo(
    () => orderPullRequestComments(detail.comments, order),
    [detail.comments, order],
  );
  return (
    <section>
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-sm font-medium">Conversation</h3>
        {detail.comments.length > 1 && (
          <Select
            value={order}
            onValueChange={(value) =>
              onOrderChange(value as PullRequestCommentOrder)
            }
          >
            <SelectTrigger
              size="sm"
              className="w-32"
              aria-label="Comment order"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="newest">Newest first</SelectItem>
              <SelectItem value="oldest">Oldest first</SelectItem>
            </SelectContent>
          </Select>
        )}
      </div>
      {detail.comments.length === 0 ? (
        <p className="mt-2 text-xs text-muted-foreground">No comments yet.</p>
      ) : (
        <div className="mt-2 flex flex-col gap-2.5">
          {ordered.map((comment, index) => (
            <PrCommentCard
              key={
                comment.id ?? `${comment.created_at}:${comment.author}:${index}`
              }
              comment={comment}
              actions={
                comment.url ? (
                  <Button
                    type="button"
                    size="icon-xs"
                    variant="ghost"
                    className="-mr-1 -mt-1 opacity-70 group-hover/comment:opacity-100"
                    aria-label={`Open ${comment.author ?? "this comment"} on GitHub`}
                    onClick={() => void openInBrowser(comment.url!)}
                  >
                    <ExternalLink />
                  </Button>
                ) : undefined
              }
            />
          ))}
        </div>
      )}
      {detail.can_comment && (
        <div className="mt-3 flex flex-col gap-2">
          <Textarea
            value={draft}
            rows={3}
            placeholder="Leave a comment. Markdown works."
            aria-label="Comment on this pull request"
            onChange={(event) => onDraftChange(event.target.value)}
          />
          <div className="flex justify-end">
            <Button
              type="button"
              size="sm"
              disabled={Boolean(busy) || !draft.trim()}
              onClick={onComment}
            >
              {busy === "comment" && <LoaderCircle className="animate-spin" />}
              Comment
            </Button>
          </div>
        </div>
      )}
    </section>
  );
}

function PrFiles({ detail }: { detail: CodeDeliveryPullRequestDetail }) {
  if (detail.files.length === 0) {
    return (
      <p className="text-xs text-muted-foreground">
        {detail.changed_files > 0
          ? "GitHub did not return this diff. Open the pull request on GitHub to read it."
          : "No files changed."}
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs text-muted-foreground">
        {detail.changed_files} {detail.changed_files === 1 ? "file" : "files"}{" "}
        changed
        <span className="ml-2 font-mono tabular-nums">
          <span className={STATUS_TEXT.ready}>+{detail.additions}</span>{" "}
          <span className={STATUS_TEXT.critical}>−{detail.deletions}</span>
        </span>
        {detail.commits > 0 && (
          <span className="ml-2">
            across {detail.commits}{" "}
            {detail.commits === 1 ? "commit" : "commits"}
          </span>
        )}
      </p>
      {detail.files.map((file) => (
        <PrFileCard key={file.path} file={file} />
      ))}
      {detail.files_truncated && (
        <p className="text-xs text-muted-foreground">
          Only the first {detail.files.length} files are shown. Open the pull
          request on GitHub for the rest.
        </p>
      )}
    </div>
  );
}

function PrFileCard({ file }: { file: CodeDeliveryPullRequestFile }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="overflow-hidden rounded-lg border border-border-subtle">
      <button
        type="button"
        aria-expanded={open}
        className="flex w-full cursor-pointer items-center gap-2 px-3 py-2 text-left hover:bg-muted/30"
        onClick={() => setOpen((value) => !value)}
      >
        <span
          className={cn(
            "shrink-0 text-2xs font-medium uppercase",
            STATUS_TEXT[fileStatusTone(file.status)],
          )}
        >
          {fileStatusLabel(file.status)}
        </span>
        <MiddleTruncate
          className="min-w-0 flex-1 font-mono text-xs"
          text={
            file.previous_path
              ? `${file.previous_path} → ${file.path}`
              : file.path
          }
        />
        <span className="shrink-0 font-mono text-xs tabular-nums">
          <span className={STATUS_TEXT.ready}>+{file.additions}</span>{" "}
          <span className={STATUS_TEXT.critical}>−{file.deletions}</span>
        </span>
      </button>
      {open && (
        <div className="border-t border-border-subtle">
          {file.patch ? (
            <DiffPatch patch={file.patch} />
          ) : (
            <p className="px-3 py-2 text-xs text-muted-foreground">
              No text diff. The file is binary, or GitHub declined to render it.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * A unified patch, colored the way a diff should be: a quiet background tint
 * per changed line and a colored sign, with the code itself kept in the
 * text fight the tint in both themes; the tint alone carries the meaning.
 */
function DiffPatch({ patch }: { patch: string }) {
  const lines = useMemo(() => patch.split("\n"), [patch]);
  return (
    <pre className="overflow-x-auto py-1 font-mono text-xs leading-[1.45]">
      {lines.map((line, index) => {
        const kind =
          line.startsWith("+") && !line.startsWith("+++")
            ? "add"
            : line.startsWith("-") && !line.startsWith("---")
              ? "remove"
              : line.startsWith("@@")
                ? "hunk"
                : "context";
        return (
          <code
            key={index}
            // `w-max min-w-full`: a block child of a scrolling <pre> otherwise
            // sizes to the visible width, so long lines ran past their own
            // background and scrolling right left colored stubs behind.
            className={cn(
              "block w-max min-w-full px-3 whitespace-pre",
              kind === "add" && "bg-success/10",
              kind === "remove" && "bg-critical/10",
              kind === "hunk" && "bg-info/10 text-info-foreground-muted",
            )}
          >
            {kind === "add" || kind === "remove" ? (
              <>
                <span
                  className={kind === "add" ? "text-success" : "text-critical"}
                >
                  {line[0]}
                </span>
                {line.slice(1)}
              </>
            ) : (
              line || " "
            )}
          </code>
        );
      })}
    </pre>
  );
}

function PrChecks({ checks }: { checks: readonly CodeDeliveryCheck[] }) {
  const counts = checkCounts({ checks });
  if (checks.length === 0) {
    return <p className="text-xs text-muted-foreground">No checks reported.</p>;
  }
  // Failures first: the reason a reader opened this tab is almost always at
  // the bottom of GitHub's own list.
  const ordered = [...checks].sort(
    (left, right) => bucketRank(left.bucket) - bucketRank(right.bucket),
  );
  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs text-muted-foreground">
        {counts.passing} of {counts.total} passed
        {counts.failing > 0 && `, ${counts.failing} failed`}
        {counts.pending > 0 && `, ${counts.pending} pending`}
        {counts.skipped > 0 && `, ${counts.skipped} skipped`}
      </p>
      <div className="flex flex-col rounded-lg border border-border-subtle">
        {ordered.map((check, index) => (
          <button
            key={`${check.name}:${index}`}
            type="button"
            disabled={!check.url}
            className="flex items-center gap-2 border-b border-border-subtle px-3 py-2 text-left text-xs last:border-b-0 enabled:cursor-pointer enabled:hover:bg-muted/30"
            onClick={() => check.url && void openInBrowser(check.url)}
          >
            <CheckTone bucket={check.bucket} />
            <span className="min-w-0 flex-1 truncate">{check.name}</span>
            {check.detail && (
              <span className="max-w-36 truncate text-muted-foreground">
                {check.detail}
              </span>
            )}
            {check.url && (
              <ExternalLink className="size-3 shrink-0 text-muted-foreground" />
            )}
          </button>
        ))}
      </div>
    </div>
  );
}

function bucketRank(bucket: CodeDeliveryCheck["bucket"]): number {
  if (bucket === "fail") return 0;
  if (bucket === "pending") return 1;
  if (bucket === "pass") return 2;
  return 3;
}

export function PrLifecycleIcon({
  lifecycle,
  className,
}: {
  lifecycle: PullRequestLifecycle;
  className?: string;
}) {
  const shared = cn("shrink-0", className);
  if (lifecycle === "merged") return <GitMerge className={shared} />;
  if (lifecycle === "closed")
    return <GitPullRequestClosed className={shared} />;
  if (lifecycle === "draft") return <GitPullRequestDraft className={shared} />;
  return <GitPullRequest className={shared} />;
}

/**
 * The stack chain this pull request belongs to, bottom to top — the map
 * GitHub pins to the top of a stacked pull request. Each layer links to its
 * GitHub page; the layer behind this sheet carries its own ring so the
 * reader can see where in the stack they are standing.
 */
export function StackMap({
  stack,
  currentNumber,
  url,
}: {
  stack: readonly CodeDeliveryStackMember[];
  currentNumber: number;
  url: string;
}) {
  return (
    <nav
      aria-label="Pull request stack, bottom layer first"
      className="mt-2 flex flex-wrap items-center gap-1"
    >
      {stack.map((member, index) => {
        const lifecycle = pullRequestLifecycle(member);
        const current = member.number === currentNumber;
        const memberUrl = url.replace(/\/pull\/\d+$/, `/pull/${member.number}`);
        return (
          <span key={member.number} className="flex items-center gap-1">
            {index > 0 && (
              <span
                className="text-muted-foreground/70 text-xs"
                aria-label="stacked on"
              >
                ←
              </span>
            )}
            <a
              href={memberUrl}
              title={`${lifecycle} · ${member.head_branch}`}
              onClick={(event) => {
                event.preventDefault();
                void openInBrowser(memberUrl);
              }}
              className={cn(
                "flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs tabular-nums transition-colors",
                current
                  ? "border-border bg-muted font-medium text-foreground"
                  : "border-transparent text-muted-foreground hover:bg-muted/60 hover:text-foreground",
              )}
            >
              <PrLifecycleIcon
                lifecycle={lifecycle}
                className={cn(
                  "size-3",
                  STATUS_MARK[PULL_REQUEST_LIFECYCLE_TONE[lifecycle]],
                )}
              />
              #{member.number}
              {current && <span className="sr-only">(this pull request)</span>}
            </a>
          </span>
        );
      })}
    </nav>
  );
}

export function CheckTone({
  bucket,
}: {
  bucket: "pass" | "pending" | "fail" | "skipped";
}) {
  if (bucket === "pass") {
    return <Check className={cn("size-3.5 shrink-0", STATUS_MARK.ready)} />;
  }
  if (bucket === "fail") {
    return <X className={cn("size-3.5 shrink-0", STATUS_MARK.critical)} />;
  }
  if (bucket === "pending") {
    return <Play className={cn("size-3.5 shrink-0", STATUS_MARK.pending)} />;
  }
  return <CircleDot className={cn("size-3.5 shrink-0", STATUS_MARK.neutral)} />;
}

function InlineDetailError({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="m-5 flex items-center justify-between gap-3 rounded-lg border border-critical-border bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted">
      <span>{message}</span>
      <Button type="button" size="xs" variant="outline" onClick={onRetry}>
        Try again
      </Button>
    </div>
  );
}

export function DetailSkeleton() {
  return (
    <div className="flex flex-col gap-4" role="status">
      <span className="sr-only">Loading details</span>
      <Skeleton className="h-8 w-36" />
      <div className="grid grid-cols-2 gap-3">
        {Array.from({ length: 6 }, (_, index) => (
          <Skeleton key={index} className="h-10" />
        ))}
      </div>
      <Skeleton className="h-28" />
      <Skeleton className="h-44" />
    </div>
  );
}

/** Shared with the delivery list so both read the same "3 days ago". */
export function relativeTime(value: string | undefined): string {
  if (!value) return "Unknown time";
  try {
    return formatDistanceToNowStrict(new Date(value), { addSuffix: true });
  } catch {
    return value;
  }
}
