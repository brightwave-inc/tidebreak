import { useEffect, useMemo, useRef, useState } from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import {
  Bot,
  Check,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CircleDot,
  ExternalLink,
  GitBranch,
  GitMerge,
  Layers,
  GitPullRequest,
  GitPullRequestClosed,
  GitPullRequestDraft,
  LoaderCircle,
  MoreHorizontal,
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
  prMergeControls,
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
  isStackedPullRequest,
  preservePullRequestStackMetadata,
} from "./pullRequestStacks";
import {
  PULL_REQUEST_LIFECYCLE_LABEL,
  PULL_REQUEST_LIFECYCLE_TONE,
  STATUS_TONE_BADGE_VARIANT,
  checkCounts,
  mergeBlockedReasons,
  prStatus,
  pullRequestLifecycle,
  pullRequestSettledAt,
  type PullRequestLifecycle,
} from "./prState";
import { STATUS_MARK, STATUS_TEXT, type StatusTone } from "./statusTone";

type MergeMethod = "squash" | "merge" | "rebase";
type DetailTab = "conversation" | "files" | "checks";

/**
 * The sheet's tabs, in the vocabulary the Delivery page already uses for a
 * band of peer views: an underline under the active one, no fill.
 *
 * `TabsTrigger`'s default pill belongs on a transparent surface — the
 * inspector and the tool card — where it has room to float. In a band closed
 * by a hairline it fills the whole height and reads as a fat button, so this
 * surface takes the same treatment as `DeliveryTab` one level up.
 */
const SHEET_TAB =
  "relative h-10 rounded-none px-3 hover:bg-transparent hover:text-foreground data-[state=active]:bg-transparent data-[state=active]:after:absolute data-[state=active]:after:right-2 data-[state=active]:after:bottom-0 data-[state=active]:after:left-2 data-[state=active]:after:h-0.5 data-[state=active]:after:rounded-full data-[state=active]:after:bg-primary";

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
          // Keep the ring off the first icon button when the sheet opens: the
          // reader came here to read, and a focus ring parked on "Open on
          // GitHub" reads as a pressed control. Focus lands on the sheet, so
          // Tab still walks the header controls first.
          onOpenAutoFocus={(event) => event.preventDefault()}
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
  loadDelayMs = 0,
  hasMergeQueue = false,
  onClose,
  onChanged,
  onDetail,
  onSummary,
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
  hasMergeQueue?: boolean;
  initialDetail?: CodeDeliveryPullRequestDetail;
  loadDelayMs?: number;
  onClose: () => void;
  onChanged: () => void;
  onDetail?: (detail: CodeDeliveryPullRequestDetail) => void;
  onSummary?: (summary: CodeDeliveryPullRequestSummary) => void;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  return (
    <DetailSheet
      label={`Pull request #${summary.number}: ${summary.title}`}
      onClose={onClose}
    >
      <PullRequestDetailPane
        client={client}
        summary={summary}
        initialDetail={initialDetail}
        loadDelayMs={loadDelayMs}
        hasMergeQueue={hasMergeQueue}
        onClose={onClose}
        onChanged={onChanged}
        onDetail={onDetail}
        onSummary={onSummary}
        onOpenWorkspace={onOpenWorkspace}
      />
    </DetailSheet>
  );
}

/**
 * The pull request itself: header, merge box, conversation, files, and checks.
 *
 * Delivery hosts this in a column beside the list. A workspace hosts the same
 * pane as a center tab. The sheet wrapper above is only for surfaces that still
 * float a dialog.
 */
export function PullRequestDetailPane({
  client,
  summary,
  initialDetail,
  loadDelayMs = 0,
  hasMergeQueue = false,
  onClose,
  onChanged,
  onDetail,
  onSummary,
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
  /** Delay an uncached read while keyboard selection is still moving. */
  loadDelayMs?: number;
  onClose?: () => void;
  onChanged: () => void;
  /** Keep loaded detail available when the reader returns to this row. */
  onDetail?: (detail: CodeDeliveryPullRequestDetail) => void;
  /** The list row should follow this summary so its action matches the pane. */
  onSummary?: (summary: CodeDeliveryPullRequestSummary) => void;
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
  const onDetailRef = useRef(onDetail);
  const onSummaryRef = useRef(onSummary);
  onDetailRef.current = onDetail;
  onSummaryRef.current = onSummary;
  activeTarget.current = summary.id;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const targetIsActive = (targetId: string) =>
    mounted.current && activeTarget.current === targetId;

  const adoptDetail = (next: CodeDeliveryPullRequestDetail) => {
    const adoptedSummary = preservePullRequestStackMetadata(
      summary,
      next.summary,
    );
    const adopted =
      adoptedSummary === next.summary
        ? next
        : { ...next, summary: adoptedSummary };
    setDetail(adopted);
    onDetailRef.current?.(adopted);
    onSummaryRef.current?.(adopted.summary);
  };

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
        adoptDetail(next);
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
      adoptDetail(initialDetail);
      setLoading(false);
    } else {
      setDetail(null);
      const timer = window.setTimeout(() => void load(), loadDelayMs);
      return () => {
        window.clearTimeout(timer);
        generation.current += 1;
      };
    }
    return () => {
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, initialDetail, loadDelayMs, summary.id]);

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
        // fresh pull request rather than trusting the head this sheet loaded
        // — the expected-head guard would refuse a stale SHA.
        const fresh = await client.getCodeDeliveryPullRequestDetail({
          repository: codeDeliveryRepositoryTarget(summary.repository),
          number: member.number,
        });
        if (fresh.summary.state !== "open" || !fresh.summary.head_sha) {
          continue;
        }
        const head = fresh.summary.head_sha;
        // One gate per hop, the same one every other merge surface uses:
        // the layer must actually be mergeable now, not just open.
        const action = prDirectMergeAction(
          deliveryPullRequestDigest(fresh.summary),
          {
            hasMergeQueue,
            suppressAutoMerge:
              isStackedPullRequest(fresh.summary) ||
              (fresh.stack?.length ?? 0) > 1,
          },
        );
        if (!action) {
          toast.warning(
            `Stack merge stopped at #${member.number}: ${
              prMergeControls(prStatus(fresh.summary).gate).explanation ??
              "it is not mergeable right now."
            }`,
          );
          return;
        }
        if (action.kind !== "merge") {
          // A queue entry hands the layer to the host without landing it, so
          // the layers above cannot merge yet. Stop here and say so — a run
          // that armed the queue has not merged the stack.
          const result = await client.runCodeDeliveryPullRequestAction({
            target: {
              repository: codeDeliveryRepositoryTarget(summary.repository),
              number: member.number,
            },
            action: {
              type: "merge",
              method: mergeMethod,
              auto: true,
              admin: false,
              expected_head_sha: head,
            },
          });
          toast[result.success ? "success" : "warning"](
            result.success
              ? `#${member.number} was added to the merge queue. The layers above follow once it lands.`
              : `Stack merge stopped at #${member.number}: ${result.message}`,
          );
          return;
        }
        const result = await client.runCodeDeliveryPullRequestAction({
          target: {
            repository: codeDeliveryRepositoryTarget(summary.repository),
            number: member.number,
          },
          action: {
            type: "merge",
            method: mergeMethod,
            auto: false,
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
    <div
      className="flex h-full min-h-0 flex-col overflow-hidden bg-background"
      data-testid="pull-request-detail-pane"
    >
      <PrDetailHeader
        summary={current}
        lifecycle={lifecycle}
        detail={detail}
        loading={loading}
        onRefresh={() => void load()}
        onClose={onClose}
      />

      {detail && (
        <PrMergeBox
          client={client}
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
          onOpenWorkspace={onOpenWorkspace}
        />
      )}

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
              <TabsList className="sticky top-0 z-10 h-10 w-full justify-start gap-1 rounded-none border-b border-border-subtle bg-background/95 px-5 backdrop-blur">
                <TabsTrigger value="conversation" className={SHEET_TAB}>
                  Conversation
                  {detail.comments.length > 0 && (
                    <span className="text-muted-foreground tabular-nums">
                      {detail.comments.length}
                    </span>
                  )}
                </TabsTrigger>
                <TabsTrigger value="files" className={SHEET_TAB}>
                  Files
                  <span className="text-muted-foreground tabular-nums">
                    {detail.changed_files}
                  </span>
                </TabsTrigger>
                <TabsTrigger value="checks" className={SHEET_TAB}>
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
                      {counts.total - counts.pending}/{counts.total}
                    </span>
                  )}
                </TabsTrigger>
              </TabsList>

              <TabsContent
                value="conversation"
                className="mt-0 flex flex-col gap-6 p-5"
              >
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
    </div>
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

/**
 * Identity, in one band.
 *
 * Everything that says *which* pull request this is sits here — where it
 * lives, what it is called, who opened it, which branches it spans, how big
 * it is — on a single wrapping metadata line rather than a stack of rows
 * with their own margins. What to *do* about it belongs to the merge box
 * below, so the window controls are the only buttons in this band.
 */
function PrDetailHeader({
  summary,
  lifecycle,
  detail,
  loading,
  onRefresh,
  onClose,
}: {
  summary: CodeDeliveryPullRequestSummary;
  lifecycle: PullRequestLifecycle;
  detail: CodeDeliveryPullRequestDetail | null;
  loading: boolean;
  onRefresh: () => void;
  onClose?: () => void;
}) {
  const settledAt = pullRequestSettledAt(summary);
  const assignees = detail?.assignees ?? [];
  const reviewers = detail?.requested_reviewers ?? [];
  return (
    <header
      aria-label="Pull request summary"
      className="flex shrink-0 flex-col gap-3 border-b border-border-subtle px-5 py-4"
    >
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <p className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
            <span className="truncate">
              {summary.repository.name_with_owner}
            </span>
            <span className="tabular-nums">#{summary.number}</span>
          </p>
          <h2 className="mt-1.5 text-lg font-semibold leading-snug">
            {summary.title}
          </h2>
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          <Button
            type="button"
            size="icon-xs"
            variant="ghost"
            title="Open this pull request on GitHub"
            onClick={() => void openInBrowser(summary.url)}
          >
            <ExternalLink />
            <span className="sr-only">Open on GitHub</span>
          </Button>
          <Button
            type="button"
            size="icon-xs"
            variant="ghost"
            title="Reload this pull request from GitHub"
            disabled={loading}
            onClick={onRefresh}
          >
            {loading ? (
              <LoaderCircle className="animate-spin" />
            ) : (
              <RefreshCw />
            )}
            <span className="sr-only">Refresh</span>
          </Button>
          {onClose ? (
            <Button
              type="button"
              size="icon-xs"
              variant="ghost"
              onClick={onClose}
            >
              <X />
              <span className="sr-only">Close pull request details</span>
            </Button>
          ) : null}
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5 text-xs text-muted-foreground">
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
        <span className="flex items-center gap-1.5">
          <GithubAvatar
            login={summary.author}
            url={summary.author_avatar_url}
            className="size-4"
          />
          <span className="font-medium text-foreground">
            {summary.author ?? "Unknown"}
          </span>
          <span>opened {relativeTime(summary.created_at)}</span>
        </span>
        {settledAt && (
          <>
            <MetaDot />
            <span>
              {lifecycle === "merged" ? "merged" : "closed"}{" "}
              {relativeTime(settledAt)}
              {detail?.merged_by ? ` by ${detail.merged_by}` : ""}
            </span>
          </>
        )}
        <MetaDot />
        <span className="flex min-w-0 items-center gap-1.5 font-mono">
          <GitBranch className="size-3 shrink-0" />
          <MiddleTruncate className="min-w-0" text={summary.base_branch} />
          <span aria-hidden>←</span>
          <MiddleTruncate className="min-w-0" text={summary.head_branch} />
        </span>
        {detail && (
          <>
            <MetaDot />
            <DiffStat
              additions={detail.additions}
              deletions={detail.deletions}
            />
          </>
        )}
      </div>

      {detail?.stack && detail.stack.length > 1 && (
        <StackMap
          stack={detail.stack}
          currentNumber={summary.number}
          url={summary.url}
        />
      )}

      {(summary.labels.length > 0 ||
        assignees.length > 0 ||
        reviewers.length > 0) && (
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
          {summary.labels.length > 0 && (
            <div className="flex flex-wrap items-center gap-1.5">
              {summary.labels.map((label) => (
                <Badge key={label} variant="outline" size="sm">
                  {label}
                </Badge>
              ))}
            </div>
          )}
          <PeopleGroup label="Assignees" logins={assignees} />
          <PeopleGroup label="Reviewers" logins={reviewers} />
        </div>
      )}
    </header>
  );
}

/**
 * The people attached to a pull request, under the word for what they are.
 *
 * Assignees and reviewers are the same shape — an avatar and a login — so
 * without the label a row of faces says nothing about who is expected to do
 * what.
 */
function PeopleGroup({
  label,
  logins,
}: {
  label: string;
  logins: readonly string[];
}) {
  if (logins.length === 0) return null;
  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="text-2xs font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
      {logins.map((login) => (
        <span
          key={login}
          className="flex items-center gap-1.5 text-xs text-muted-foreground"
        >
          <GithubAvatar login={login} className="size-4" />
          {login}
        </span>
      ))}
    </div>
  );
}

/** The one separator this sheet uses between metadata facts. */
function MetaDot() {
  return (
    <span aria-hidden className="text-muted-foreground/50">
      ·
    </span>
  );
}

/**
 * How big the change is, painted the same way everywhere it appears — the
 * header, the Files summary, and every file row.
 */
export function DiffStat({
  additions,
  deletions,
  className,
}: {
  additions: number;
  deletions: number;
  className?: string;
}) {
  return (
    <span className={cn("font-mono text-xs tabular-nums", className)}>
      <span className={STATUS_TEXT.ready}>+{additions}</span>{" "}
      <span className={STATUS_TEXT.critical}>−{deletions}</span>
    </span>
  );
}

/**
 * A titled block of content inside the sheet.
 *
 * The heading is an eyebrow, not a second title: it names the block without
 * competing with the pull request's own name, and every block on the surface
 * gets the same rhythm above and below it.
 */
function DetailSection({
  title,
  action,
  children,
}: {
  title: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className="flex flex-col gap-2.5">
      <div className="flex min-h-7 items-center justify-between gap-2">
        <h3 className="text-2xs font-medium uppercase tracking-wide text-muted-foreground">
          {title}
        </h3>
        {action}
      </div>
      {children}
    </section>
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

/**
 * What stands between this pull request and the base branch, and every move
 * that changes it.
 *
 * The band always leads with the gate — the same answer `prState.ts` gives
 * the delivery list, so a row and its sheet never disagree — then the exact
 * reasons underneath, then the controls. Leading with the state is what keeps
 * a pull request with nothing to do from rendering as a lone overflow button:
 * "Checks running" is the answer, and the buttons are the footnote.
 *
 * It sits above the tabs rather than inside Conversation because it is the
 * reason the sheet is open. Reading the diff should not cost you the merge.
 */
function PrMergeBox({
  client,
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
  onOpenWorkspace,
}: {
  client: Pick<ApiClient, "createCodeWorkspace" | "writeCodeCheckLogs">;
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
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const [confirmingStackMerge, setConfirmingStackMerge] = useState(false);
  const [confirmingCreateStack, setConfirmingCreateStack] = useState(false);
  const status = prStatus(summary);
  const settled =
    status.lifecycle === "merged" || status.lifecycle === "closed";
  // The headline already names the gate. A single blocker only ever restates
  // it in a longer sentence, so the list earns its space from two up, where
  // it says what else must clear before the headline moves.
  const allBlockers =
    status.lifecycle === "open" ? mergeBlockedReasons(summary) : [];
  const blockers = allBlockers.length > 1 ? allBlockers : [];
  const mergeAction =
    detail.can_merge && summary.head_sha
      ? prDirectMergeAction(deliveryPullRequestDigest(summary), {
          hasMergeQueue,
          suppressAutoMerge:
            isStackedPullRequest(summary) || (detail.stack?.length ?? 0) > 1,
        })
      : null;
  const canRerun = detail.can_rerun_failed && workflowRunIds.length > 0;
  const canAdminMerge = detail.can_merge && Boolean(summary.head_sha);
  // The stack offer lands this pull request and every unmerged layer below
  // it, in order — GitHub's own "merge the latest ready pull request" move.
  // Merged layers below are skipped (already landed), a draft layer blocks
  // everything above it, and the run stops at this pull request: layers
  // above it are not the reader's to land from here. The offer itself is
  // gated by the same decision every merge surface uses, and while it is on
  // the table it replaces the single-layer merge — merging one layer of a
  // live stack alone is the half-measure the stack exists to avoid.
  const mergeableStackLayers: CodeDeliveryStackMember[] = [];
  let stackRunReachesThis = false;
  for (const member of detail.stack ?? []) {
    if (member.state !== "open") continue;
    if (member.draft) break;
    mergeableStackLayers.push(member);
    if (member.number === summary.number) {
      stackRunReachesThis = true;
      break;
    }
  }
  const canMergeStack =
    detail.can_merge &&
    Boolean(summary.head_sha) &&
    mergeAction !== null &&
    stackRunReachesThis &&
    mergeableStackLayers.length >= 2;
  const showSingleMerge = !canMergeStack && mergeAction !== null;
  // A chain this page inferred but the host has no stack for. Registering it
  // moves the ordering, the retargeting, and the whole-chain merge to
  // GitHub; leaving it unregistered keeps the accident open, where merging a
  // layer lands it into the branch below instead of the default branch.
  const unregisteredStack =
    detail.can_merge && summary.unregistered_stack_numbers !== undefined
      ? summary.unregistered_stack_numbers
      : null;
  const anyAction =
    detail.can_mark_ready ||
    mergeAction !== null ||
    canMergeStack ||
    unregisteredStack !== null ||
    canAdminMerge ||
    detail.can_close ||
    detail.can_reopen ||
    canRerun;
  // A settled pull request with nothing to do and nowhere to go says its
  // outcome in the header already; a second band repeating it is noise.
  if (settled && !anyAction && summary.workspace_links.length === 0) {
    return null;
  }
  // The header badge already carries the bare lifecycle word, so a headline
  // that would only repeat it says what that state means instead.
  const headline =
    status.lifecycle === "merged"
      ? `Merged into ${summary.base_branch}`
      : status.lifecycle === "closed"
        ? "Closed without merging"
        : status.lifecycle === "draft"
          ? "Not ready for review"
          : status.headline.label;
  return (
    <section
      aria-label="Merge status and actions"
      className="flex shrink-0 flex-col gap-2.5 border-b border-border-subtle px-5 py-3"
    >
      <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-2.5">
        <div className="flex min-w-0 flex-1 items-start gap-2">
          <GateMark tone={status.headline.tone} />
          <div className="flex min-w-0 flex-col gap-1">
            <p
              className={cn(
                "text-sm font-medium",
                STATUS_TEXT[status.headline.tone],
              )}
            >
              {headline}
            </p>
            {blockers.length > 0 && (
              <ul className="flex flex-col gap-0.5 text-xs text-muted-foreground">
                {blockers.map((reason) => (
                  <li key={reason}>{reason}</li>
                ))}
              </ul>
            )}
          </div>
        </div>
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
              <MergeMethodSelect
                value={mergeMethod}
                onChange={onMergeMethodChange}
              />
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
              <MergeMethodSelect
                value={mergeMethod}
                onChange={onMergeMethodChange}
              />
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
            </>
          )}
          {unregisteredStack !== null && (
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={Boolean(busy)}
              onClick={() => setConfirmingCreateStack(true)}
            >
              <Layers />
              Create stack ({unregisteredStack.length} layers)
            </Button>
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
      </div>

      {confirmingStackMerge && canMergeStack && (
        <ConfirmStrip
          onConfirm={() => {
            setConfirmingStackMerge(false);
            onMergeStack(mergeableStackLayers);
          }}
          confirmLabel="Merge stack"
          busy={Boolean(busy)}
          onCancel={() => setConfirmingStackMerge(false)}
        >
          Lands #{summary.number} and the {mergeableStackLayers.length - 1}{" "}
          unmerged layer
          {mergeableStackLayers.length - 1 === 1 ? "" : "s"} below it (
          {mergeableStackLayers.map((member) => `#${member.number}`).join(", ")}
          ), bottom to top, each with{" "}
          {hasMergeQueue
            ? "the merge queue — the first layer joins the queue and the rest follow once it lands"
            : `a direct ${mergeMethod} merge`}
          . The chain stops at the first layer that cannot merge; draft layers
          stay open.
        </ConfirmStrip>
      )}

      {confirmingCreateStack && unregisteredStack !== null && (
        <ConfirmStrip
          onConfirm={() => {
            setConfirmingCreateStack(false);
            onRun("create-stack", {
              type: "create_stack",
              numbers: [...unregisteredStack],
            });
          }}
          confirmLabel="Create stack"
          busy={Boolean(busy)}
          onCancel={() => setConfirmingCreateStack(false)}
        >
          Registers {unregisteredStack.map((number) => `#${number}`).join(", ")}{" "}
          as a GitHub stack, bottom to top. GitHub then owns the ordering:
          merging a layer lands everything below it on{" "}
          {summary.repository.default_branch ?? "the default branch"}, and the
          layers above rebase and retarget on their own.
        </ConfirmStrip>
      )}

      {confirmingAdminMerge && canAdminMerge && (
        <ConfirmStrip
          tone="critical"
          icon={<ShieldAlert className="mt-0.5 size-3.5 shrink-0" />}
          onConfirm={onAdminMergeConfirm}
          confirmLabel="Admin merge"
          confirmVariant="destructive"
          busy={Boolean(busy)}
          onCancel={onAdminMergeCancel}
        >
          Admin merge lands this pull request now and skips any reviews and
          checks the branch still requires. GitHub records the bypass under your
          account.
        </ConfirmStrip>
      )}

      <PrWorkspaceRow
        client={client}
        summary={summary}
        onOpenWorkspace={onOpenWorkspace}
      />
    </section>
  );
}

/** The merge method, offered the same way wherever a merge button appears. */
function MergeMethodSelect({
  value,
  onChange,
}: {
  value: MergeMethod;
  onChange: (method: MergeMethod) => void;
}) {
  return (
    <Select
      value={value}
      onValueChange={(next) => onChange(next as MergeMethod)}
    >
      <SelectTrigger size="sm" className="w-28" aria-label="Merge method">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="squash">Squash</SelectItem>
        <SelectItem value="merge">Merge</SelectItem>
        <SelectItem value="rebase">Rebase</SelectItem>
      </SelectContent>
    </Select>
  );
}

/**
 * An inline "are you sure" inside the merge box.
 *
 * Inline rather than a dialog: the sheet is already a modal, and a second
 * Radix modal stacked on it shares the dismiss layer and the body
 * pointer-events lock — the class of bug #2537 hit with a dialog over a
 * dialog.
 */
function ConfirmStrip({
  tone = "neutral",
  icon,
  children,
  confirmLabel,
  confirmVariant = "default",
  busy,
  onConfirm,
  onCancel,
}: {
  tone?: "neutral" | "critical";
  icon?: React.ReactNode;
  children: React.ReactNode;
  confirmLabel: string;
  confirmVariant?: "default" | "destructive";
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div
      className={cn(
        "flex flex-col gap-2 rounded-md border p-2.5",
        tone === "critical"
          ? "border-critical-border bg-critical-background/40"
          : "border-border-subtle bg-muted/30",
      )}
    >
      <p
        className={cn(
          "flex items-start gap-1.5 text-xs",
          tone === "critical"
            ? "text-critical-foreground-muted"
            : "text-muted-foreground",
        )}
      >
        {icon}
        <span>{children}</span>
      </p>
      <div className="flex items-center gap-2">
        <Button
          type="button"
          size="xs"
          variant={confirmVariant}
          disabled={busy}
          onClick={onConfirm}
        >
          {busy && <LoaderCircle className="animate-spin" />}
          {confirmLabel}
        </Button>
        <Button
          type="button"
          size="xs"
          variant="ghost"
          disabled={busy}
          onClick={onCancel}
        >
          Cancel
        </Button>
      </div>
    </div>
  );
}

/** The mark that carries the gate on its own, ahead of the headline. */
function GateMark({ tone }: { tone: StatusTone }) {
  const shared = cn("mt-0.5 size-4 shrink-0", STATUS_MARK[tone]);
  if (tone === "ready") return <CircleCheck className={shared} />;
  if (tone === "critical") return <CircleAlert className={shared} />;
  if (tone === "warning") return <CircleAlert className={shared} />;
  if (tone === "merged") return <GitMerge className={shared} />;
  if (tone === "pending") return <CircleDashed className={shared} />;
  return <CircleDot className={shared} />;
}

/**
 * Where the work happens: the workspaces carrying this pull request, and the
 * agent that can take its remaining chores. Both belong beside the merge
 * controls — they are the moves that change the state above them.
 */
function PrWorkspaceRow({
  client,
  summary,
  onOpenWorkspace,
}: {
  client: Pick<ApiClient, "createCodeWorkspace" | "writeCodeCheckLogs">;
  summary: CodeDeliveryPullRequestSummary;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const agentMenu = (
    <PrAgentMenu
      client={client}
      summary={summary}
      onOpenWorkspace={onOpenWorkspace}
    />
  );
  return (
    <div className="flex flex-wrap items-center gap-x-2 gap-y-2 border-t border-border-subtle pt-2.5">
      {summary.workspace_links.map((workspace) => (
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
      {agentMenu}
      {summary.workspace_links.length === 0 && (
        <p className="w-full text-xs text-muted-foreground">
          Not linked to a Tidebreak workspace. These GitHub actions still work;
          code changes need a workspace.
        </p>
      )}
    </div>
  );
}

function PrDescription({ body }: { body: string }) {
  const trimmed = body.trim();
  return (
    <DetailSection title="Description">
      {trimmed ? (
        <div className="review-comment-markdown text-md leading-5">
          <MessageMarkdown>
            {expandGithubEmojiShortcodes(trimmed)}
          </MessageMarkdown>
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">
          No description provided.
        </p>
      )}
    </DetailSection>
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
    <DetailSection
      title="Conversation"
      action={
        detail.comments.length > 1 ? (
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
        ) : undefined
      }
    >
      {detail.comments.length === 0 ? (
        <p className="text-xs text-muted-foreground">No comments yet.</p>
      ) : (
        <div className="flex flex-col gap-2.5">
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
        <div className="mt-1 flex flex-col gap-2">
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
    </DetailSection>
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
    <DetailSection
      title="Changed files"
      action={
        <span className="flex items-center gap-2.5 text-xs text-muted-foreground">
          <span>
            {detail.changed_files}{" "}
            {detail.changed_files === 1 ? "file" : "files"} changed
            {detail.commits > 0 &&
              ` across ${detail.commits} ${
                detail.commits === 1 ? "commit" : "commits"
              }`}
          </span>
          <DiffStat additions={detail.additions} deletions={detail.deletions} />
        </span>
      }
    >
      {detail.files.map((file) => (
        <PrFileCard key={file.path} file={file} />
      ))}
      {detail.files_truncated && (
        <p className="text-xs text-muted-foreground">
          Only the first {detail.files.length} files are shown. Open the pull
          request on GitHub for the rest.
        </p>
      )}
    </DetailSection>
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
            "w-16 shrink-0 text-2xs font-medium uppercase",
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
          <DiffStat additions={file.additions} deletions={file.deletions} />
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
    <DetailSection
      title="Checks"
      action={
        <span className="text-xs text-muted-foreground">
          {`${counts.passing} of ${counts.total} passed${
            counts.failing > 0 ? `, ${counts.failing} failed` : ""
          }${counts.pending > 0 ? `, ${counts.pending} pending` : ""}${
            counts.skipped > 0 ? `, ${counts.skipped} skipped` : ""
          }`}
        </span>
      }
    >
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
    </DetailSection>
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
    return (
      <CircleDashed className={cn("size-3.5 shrink-0", STATUS_MARK.pending)} />
    );
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
