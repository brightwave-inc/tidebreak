import { useEffect, useMemo, useRef, useState } from "react";
import {
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
  X,
} from "lucide-react";
import { formatDistanceToNowStrict } from "date-fns";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type {
  CodeDeliveryCheck,
  CodeDeliveryPullRequestAction,
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestFile,
  CodeDeliveryPullRequestSummary,
} from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
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
import { codeDeliveryRepositoryTarget } from "./CodeDeliveryStore";
import { MiddleTruncate } from "./MiddleTruncate";
import { PrCommentCard } from "./PrCommentCard";
import {
  checkCounts,
  expandGithubEmojiShortcodes,
  fileStatusLabel,
  fileStatusTone,
  githubAvatarUrl,
  mergeBlockedReason,
  pullRequestLifecycle,
  pullRequestSettledAt,
  PULL_REQUEST_LIFECYCLE_LABEL,
  type PullRequestLifecycle,
} from "./pullRequestPresentation";
import { STATUS_MARK, STATUS_TEXT } from "./statusTone";

type MergeMethod = "squash" | "merge" | "rebase";
type DetailTab = "conversation" | "files" | "checks";

const LIFECYCLE_BADGE_VARIANT: Record<
  PullRequestLifecycle,
  "outline" | "success" | "critical" | "merged"
> = {
  draft: "outline",
  open: "success",
  merged: "merged",
  closed: "critical",
};

/**
 * The pull request, as close to its GitHub page as a desktop panel gets.
 *
 * The point is that a reader finishes here. Everything the GitHub page leads
 * with — lifecycle, who merged it and when, the branch pair, the diffstat,
 * labels and reviewers, the description as real Markdown, the conversation,
 * every check, every changed file — is on this surface, and the actions that
 * page offers (ready, merge, auto-merge, rerun, close, reopen, comment) run
 * from it. "Open on GitHub" stays, but as an escape hatch rather than the
 * only way to see what happened.
 */
export function PullRequestDetailPanel({
  client,
  summary,
  onClose,
  onChanged,
  onOpenWorkspace,
}: {
  client: Pick<
    ApiClient,
    "getCodeDeliveryPullRequestDetail" | "runCodeDeliveryPullRequestAction"
  >;
  summary: CodeDeliveryPullRequestSummary;
  onClose: () => void;
  onChanged: () => void;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const [detail, setDetail] = useState<CodeDeliveryPullRequestDetail | null>(
    null,
  );
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [tab, setTab] = useState<DetailTab>("conversation");
  const [mergeMethod, setMergeMethod] = useState<MergeMethod>("squash");
  const [draftComment, setDraftComment] = useState("");
  const generation = useRef(0);

  const load = async () => {
    const token = ++generation.current;
    setLoading(true);
    setError(null);
    try {
      const next = await client.getCodeDeliveryPullRequestDetail({
        repository: codeDeliveryRepositoryTarget(summary.repository),
        number: summary.number,
      });
      if (token === generation.current) setDetail(next);
    } catch (caught) {
      if (token === generation.current) {
        setError(
          friendlyErrorMessage(caught, "Could not load this pull request."),
        );
      }
    } finally {
      if (token === generation.current) setLoading(false);
    }
  };

  useEffect(() => {
    setTab("conversation");
    setDraftComment("");
    void load();
    return () => {
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, summary.id]);

  const runAction = async (
    name: string,
    action: CodeDeliveryPullRequestAction,
  ) => {
    if (busy) return;
    setBusy(name);
    try {
      const result = await client.runCodeDeliveryPullRequestAction({
        target: {
          repository: codeDeliveryRepositoryTarget(summary.repository),
          number: summary.number,
        },
        action,
      });
      toast.success(result.message);
      if (action.type === "comment") setDraftComment("");
      await load();
      onChanged();
    } catch (caught) {
      toast.error(
        friendlyErrorMessage(caught, "The pull request action failed."),
      );
    } finally {
      setBusy(null);
    }
  };

  // Prefer the freshly loaded summary: an action just changed it, and the row
  // behind this panel may still be a poll behind.
  const current = detail?.summary ?? summary;
  const lifecycle = pullRequestLifecycle(current);
  const counts = checkCounts(current.checks);
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

  return (
    <aside className="flex min-h-0 w-full flex-col border-l border-border-subtle bg-background lg:w-auto">
      <PrDetailHeader
        summary={current}
        lifecycle={lifecycle}
        detail={detail}
        onClose={onClose}
      />

      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border-subtle px-4 py-2.5">
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
            onClick={() => onOpenWorkspace(workspace.workspace_id)}
          >
            <GitBranch />
            Open {workspace.title}
          </Button>
        ))}
        <Button
          type="button"
          size="xs"
          variant="ghost"
          disabled={loading}
          onClick={() => void load()}
        >
          {loading ? <LoaderCircle className="animate-spin" /> : <RefreshCw />}
          Refresh
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {loading && !detail ? (
          <div className="p-4">
            <DetailSkeleton />
          </div>
        ) : error ? (
          <InlineDetailError message={error} onRetry={() => void load()} />
        ) : detail ? (
          <Tabs
            value={tab}
            onValueChange={(value) => setTab(value as DetailTab)}
          >
            <TabsList className="sticky top-0 z-10 w-full justify-start rounded-none border-b border-border-subtle bg-background/95 px-4 backdrop-blur">
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
                      counts.failed > 0
                        ? STATUS_TEXT.critical
                        : counts.pending > 0
                          ? STATUS_TEXT.pending
                          : STATUS_TEXT.ready,
                    )}
                  >
                    {counts.passed}/{counts.total}
                  </span>
                )}
              </TabsTrigger>
            </TabsList>

            <TabsContent
              value="conversation"
              className="mt-0 flex flex-col gap-5 p-4"
            >
              <PrActions
                detail={detail}
                summary={current}
                busy={busy}
                mergeMethod={mergeMethod}
                onMergeMethodChange={setMergeMethod}
                workflowRunIds={workflowRunIds}
                onRun={(name, action) => void runAction(name, action)}
              />
              <PrDescription body={detail.body} />
              <PrConversation
                detail={detail}
                busy={busy}
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

            <TabsContent value="files" className="mt-0 p-4">
              <PrFiles detail={detail} />
            </TabsContent>

            <TabsContent value="checks" className="mt-0 p-4">
              <PrChecks checks={current.checks} />
            </TabsContent>
          </Tabs>
        ) : null}
      </div>
    </aside>
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
    <div className="shrink-0 border-b border-border-subtle px-4 py-3">
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
        <Badge variant={LIFECYCLE_BADGE_VARIANT[lifecycle]} size="sm">
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
          <span className="font-mono text-[11px] tabular-nums">
            <span className={STATUS_TEXT.ready}>+{detail.additions}</span>{" "}
            <span className={STATUS_TEXT.critical}>−{detail.deletions}</span>
          </span>
        )}
      </div>

      <p className="mt-2 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs text-muted-foreground">
        <Avatar
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

      <p className="mt-1.5 flex min-w-0 items-center gap-1.5 font-mono text-[11px] text-muted-foreground">
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
              className="flex items-center gap-1 text-[11px] text-muted-foreground"
            >
              <Avatar login={login} className="size-4" />
              {login}
            </span>
          ))}
          {detail?.requested_reviewers.map((login) => (
            <span
              key={`reviewer:${login}`}
              className="flex items-center gap-1 text-[11px] text-muted-foreground"
              title={`Review requested from ${login}`}
            >
              <Avatar login={login} className="size-4" />
              {login}
              <span className="text-[10px]">(review requested)</span>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function PrActions({
  detail,
  summary,
  busy,
  mergeMethod,
  onMergeMethodChange,
  workflowRunIds,
  onRun,
}: {
  detail: CodeDeliveryPullRequestDetail;
  summary: CodeDeliveryPullRequestSummary;
  busy: string | null;
  mergeMethod: MergeMethod;
  onMergeMethodChange: (method: MergeMethod) => void;
  workflowRunIds: number[];
  onRun: (name: string, action: CodeDeliveryPullRequestAction) => void;
}) {
  const blocked = mergeBlockedReason(summary);
  const canRerun = detail.can_rerun_failed && workflowRunIds.length > 0;
  const anyAction =
    detail.can_mark_ready ||
    detail.can_merge ||
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
        {detail.can_merge && summary.head_sha && (
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
              disabled={Boolean(busy) || Boolean(blocked)}
              onClick={() =>
                onRun("merge", {
                  type: "merge",
                  method: mergeMethod,
                  auto: false,
                  expected_head_sha: summary.head_sha!,
                })
              }
            >
              {busy === "merge" && <LoaderCircle className="animate-spin" />}
              <GitMerge />
              Merge
            </Button>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={Boolean(busy) || summary.auto_merge_enabled}
              onClick={() =>
                onRun("auto-merge", {
                  type: "merge",
                  method: mergeMethod,
                  auto: true,
                  expected_head_sha: summary.head_sha!,
                })
              }
            >
              {busy === "auto-merge" && (
                <LoaderCircle className="animate-spin" />
              )}
              {summary.auto_merge_enabled
                ? "Auto-merge on"
                : "Enable auto-merge"}
            </Button>
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
        {detail.can_close && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                disabled={Boolean(busy)}
                aria-label="More pull request actions"
              >
                {busy === "close" ? (
                  <LoaderCircle className="animate-spin" />
                ) : (
                  <MoreHorizontal />
                )}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem
                variant="destructive"
                onSelect={() => onRun("close", { type: "close" })}
              >
                <GitPullRequestClosed />
                Close without merging
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
      {blocked && detail.can_merge && (
        <p className="mt-2.5 flex items-start gap-1.5 border-t border-border-subtle pt-2.5 text-xs text-warning-foreground">
          <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
          {blocked}
        </p>
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
        <div className="review-comment-markdown mt-2 text-[13px] leading-5">
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
  draft,
  onDraftChange,
  onComment,
}: {
  detail: CodeDeliveryPullRequestDetail;
  busy: string | null;
  draft: string;
  onDraftChange: (value: string) => void;
  onComment: () => void;
}) {
  return (
    <section>
      <h3 className="text-sm font-medium">Conversation</h3>
      {detail.comments.length === 0 ? (
        <p className="mt-2 text-xs text-muted-foreground">No comments yet.</p>
      ) : (
        <div className="mt-2 flex flex-col gap-2.5">
          {detail.comments.map((comment, index) => (
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
            "shrink-0 text-[10px] font-medium uppercase",
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
        <span className="shrink-0 font-mono text-[11px] tabular-nums">
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

/** A unified patch, colored the way a diff should be. */
function DiffPatch({ patch }: { patch: string }) {
  const lines = useMemo(() => patch.split("\n"), [patch]);
  return (
    <pre className="overflow-x-auto py-1 font-mono text-[11px] leading-[1.45]">
      {lines.map((line, index) => (
        <code
          key={index}
          className={cn(
            "block px-3 whitespace-pre",
            line.startsWith("+") &&
              !line.startsWith("+++") &&
              "bg-success-background text-success-foreground-muted",
            line.startsWith("-") &&
              !line.startsWith("---") &&
              "bg-critical-background text-critical-foreground-muted",
            line.startsWith("@@") && "text-info-foreground",
          )}
        >
          {line || " "}
        </code>
      ))}
    </pre>
  );
}

function PrChecks({ checks }: { checks: readonly CodeDeliveryCheck[] }) {
  const counts = checkCounts(checks);
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
        {counts.passed} of {counts.total} passed
        {counts.failed > 0 && `, ${counts.failed} failed`}
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

function Avatar({
  login,
  url,
  className,
}: {
  login: string | undefined;
  url?: string;
  className?: string;
}) {
  const [failed, setFailed] = useState(false);
  const source = url ?? githubAvatarUrl(login);
  if (!source || failed) {
    return (
      <span
        className={cn(
          "grid shrink-0 place-items-center rounded-full bg-muted text-[8px] font-semibold uppercase text-muted-foreground",
          className,
        )}
        aria-hidden
      >
        {(login ?? "?").slice(0, 2)}
      </span>
    );
  }
  return (
    <img
      src={source}
      alt=""
      className={cn("shrink-0 rounded-full object-cover", className)}
      onError={() => setFailed(true)}
    />
  );
}

function InlineDetailError({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="m-4 flex items-center justify-between gap-3 rounded-lg border border-critical-border bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted">
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
