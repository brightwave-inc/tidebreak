import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  Check,
  CircleDashed,
  ExternalLink,
  Files,
  GitBranch,
  GitMerge,
  GitPullRequest,
  MessageSquare,
  RefreshCw,
  X,
} from "lucide-react";
import { toast } from "sonner";

import { HttpError, type ApiClient } from "../api/client";
import type {
  CodePrMergeMethod,
  CodeWorkspaceSnapshot,
  PullRequestCheck,
  PullRequestComment,
  PullRequestDigest,
} from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { WithTooltip } from "@/components/ui/tooltip";
import { useConfirm } from "@/components/ConfirmDialog";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { openExternal } from "@/host";
import type { CodeTranscriptItem } from "./CodeSessionReducer";
import { useCodeUiStore } from "./CodeUiStore";
import { DiffPanel } from "./DiffPanel";
import { FilesPanel } from "./FilesPanel";
import { FOCUS_RING, FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";
import { PrCard } from "./PrCard";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { PR_ICON_TONE_CLASSES, prTone, prToneLabel } from "./workspaceCards";

type InspectorTab = "files" | "source" | "pr";

const TAB_TRIGGER_CLASS =
  "text-muted-foreground hover:bg-transparent hover:text-foreground grid size-8 place-items-center rounded-lg px-0 py-0";

const TAB_TRIGGER_SELECTED_CLASS =
  "bg-foreground/10 text-foreground hover:bg-foreground/10 hover:text-foreground data-[state=active]:bg-foreground/10 data-[state=active]:text-foreground shadow-[inset_0_-2px_0_0_currentColor]";

/**
 * Right workspace rail: Files, Source control, and Pull request as icon tabs.
 *
 * Files is a nested worktree explorer. Source control is the worktree patch
 * plus commit, push, and PR creation. Pull request carries the PR's own
 * life: status, checks, and review comments once one exists.
 */
export function CodeInspector({
  client,
  workspaceId,
  workspace,
  contentRevision,
  onOpenFile,
  onOpenDiff,
}: {
  client: ApiClient;
  workspaceId: string;
  workspace: CodeWorkspaceSnapshot | null;
  contentRevision: number;
  onOpenFile?: (path: string) => void;
  onOpenDiff?: (path: string) => void;
}) {
  const digest = useCodeUpdatesStore((state) => state.byWorkspace[workspaceId]);
  const pr = digest?.pr_state ?? workspace?.pr;
  const scope = useCodeUiStore((state) => state.inspectorScope);
  const setInspectorScope = useCodeUiStore((state) => state.setInspectorScope);
  const [tab, setTab] = useState<InspectorTab>(scope ? "source" : "files");
  const [file, setFile] = useState<string | undefined>();
  const active = workspace?.status !== "archived";
  const turnId = scope?.turnId;

  useEffect(() => {
    if (!scope) return;
    setTab("source");
    setFile(undefined);
  }, [scope]);

  function openFile(next: string) {
    if (onOpenFile) {
      onOpenFile(next);
      return;
    }
    setFile(next);
    setTab("source");
  }

  function openDiff(next: string) {
    if (onOpenDiff) {
      onOpenDiff(next);
      return;
    }
    setFile(next);
    setTab("source");
  }

  return (
    <aside
      className="flex h-full min-h-0 w-full min-w-0 flex-col overflow-hidden"
      aria-label="Workspace surfaces"
      data-testid="code-inspector"
    >
      <Tabs
        value={tab}
        onValueChange={(next) => setTab(next as InspectorTab)}
        className="flex min-h-0 flex-1 flex-col overflow-hidden"
      >
        <header className="flex h-12 shrink-0 items-center gap-1 border-b px-2">
          <TabsList className="h-auto justify-start gap-0.5 bg-transparent p-0">
            <InspectorTabTrigger value="files" label="Files" selected={tab === "files"}>
              <Files className="size-3.5" />
            </InspectorTabTrigger>
            <InspectorTabTrigger
              value="source"
              label="Source control"
              selected={tab === "source"}
            >
              <GitBranch className="size-3.5" />
            </InspectorTabTrigger>
            <InspectorTabTrigger value="pr" label="Pull request" selected={tab === "pr"}>
              <GitPullRequest
                className={cn(
                  "size-3.5",
                  pr ? PR_ICON_TONE_CLASSES[prTone(pr)] : undefined,
                )}
              />
            </InspectorTabTrigger>
          </TabsList>
          {scope && (
            <button
              type="button"
              className={cn(
                "text-muted-foreground hover:bg-muted hover:text-foreground ml-auto cursor-pointer truncate rounded-full border px-2 py-0.5 font-mono text-[11px]",
                FOCUS_RING,
                HOVER_TINT,
              )}
              aria-label={`Clear ${scope.label} scope`}
              title={scope.label}
              onClick={() => setInspectorScope(null)}
            >
              {scope.label} ×
            </button>
          )}
        </header>
        <TabsContent
          value="files"
          className="mt-0 flex min-h-0 flex-1 flex-col overflow-hidden"
        >
          <FilesPanel
            client={client}
            workspaceId={workspaceId}
            contentRevision={contentRevision}
            selected={file}
            onOpenFile={openFile}
          />
        </TabsContent>
        <TabsContent
          value="source"
          className="mt-0 flex min-h-0 flex-1 flex-col overflow-hidden"
        >
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
            {active && (
              <div className="border-b px-3 py-3">
                <PrCard
                  client={client}
                  workspaceId={workspaceId}
                  contentRevision={contentRevision}
                  framed={false}
                />
              </div>
            )}
            <DiffPanel
              client={client}
              workspaceId={workspaceId}
              turnId={turnId}
              turnLabel={scope?.label}
              file={file}
              contentRevision={contentRevision}
              onOpenFile={openDiff}
            />
          </div>
        </TabsContent>
        <TabsContent
          value="pr"
          className="mt-0 flex min-h-0 flex-1 flex-col overflow-hidden"
        >
          <PrTab
            client={client}
            workspaceId={workspaceId}
            pr={pr}
            branch={workspace?.branch_name}
            onOpenSourceControl={() => setTab("source")}
          />
        </TabsContent>
      </Tabs>
    </aside>
  );
}

function InspectorTabTrigger({
  value,
  label,
  selected,
  children,
}: {
  value: InspectorTab;
  label: string;
  selected: boolean;
  children: ReactNode;
}) {
  return (
    <WithTooltip label={label}>
      <span className="inline-flex">
        <TabsTrigger
          value={value}
          aria-label={label}
          className={cn(TAB_TRIGGER_CLASS, selected && TAB_TRIGGER_SELECTED_CLASS)}
        >
          {children}
        </TabsTrigger>
      </span>
    </WithTooltip>
  );
}

/**
 * Ordinal label for a turn id, from the transcript's user items in order.
 * The inspector never shows a raw turn UUID.
 */
export function inspectorTurnLabel(
  items: readonly CodeTranscriptItem[],
  turnId: string,
): string {
  let ordinal = 0;
  for (const item of items) {
    if (item.kind !== "user") continue;
    ordinal += 1;
    if (item.turnId === turnId) return `Turn ${ordinal}`;
  }
  return "This turn";
}

/**
 * The pull request's own life: status, checks, review comments, and the two
 * user-initiated ways to land it. The digest arrives over the updates socket,
 * so actions only have to hit the server — the fresh state restates itself.
 */
function PrTab({
  client,
  workspaceId,
  pr,
  branch,
  onOpenSourceControl,
}: {
  client: ApiClient;
  workspaceId: string;
  pr?: PullRequestDigest;
  branch?: string;
  /** Send the reader to the tab that can actually open a pull request. */
  onOpenSourceControl?: () => void;
}) {
  const { confirm, dialog } = useConfirm();
  const [refreshing, setRefreshing] = useState(false);
  const [merging, setMerging] = useState<"merge" | "auto" | null>(null);
  const [method, setMethod] = useState<CodePrMergeMethod>("squash");
  const [mergeError, setMergeError] = useState<string | null>(null);
  const [comments, setComments] = useState<PullRequestComment[] | null>(null);
  const [commentsError, setCommentsError] = useState<string | null>(null);
  const prNumber = pr?.number;

  const loadComments = useCallback(async () => {
    if (prNumber === undefined) return;
    try {
      const snapshot = await client.getCodePrComments(workspaceId);
      setComments(snapshot.comments);
      setCommentsError(null);
    } catch (err) {
      setCommentsError(
        friendlyErrorMessage(err, "Could not load review comments"),
      );
    }
  }, [client, prNumber, workspaceId]);

  useEffect(() => {
    setComments(null);
    setCommentsError(null);
    if (prNumber === undefined) return;
    void loadComments();
  }, [loadComments, prNumber]);

  async function refresh() {
    setRefreshing(true);
    try {
      await client.refreshCodeWorkspacePr(workspaceId);
      await loadComments();
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not refresh the pull request"));
    } finally {
      setRefreshing(false);
    }
  }

  async function merge(auto: boolean) {
    if (!pr) return;
    if (!auto) {
      const ok = await confirm({
        title: `Merge #${pr.number}?`,
        description: `The pull request is ${method === "squash" ? "squash-merged" : method === "rebase" ? "rebased and merged" : "merged"} into ${pr.base_branch ?? "its base branch"} on GitHub.`,
        confirmLabel: "Merge",
      });
      if (!ok) return;
    }
    setMerging(auto ? "auto" : "merge");
    setMergeError(null);
    try {
      await client.mergeCodePr(workspaceId, { method, auto });
      toast.success(auto ? "Auto-merge enabled" : "Merged");
    } catch (err) {
      if (err instanceof HttpError && err.kind === "pr_not_mergeable") {
        setMergeError(err.message);
      } else {
        toast.error(friendlyErrorMessage(err, "Could not merge"));
      }
    } finally {
      setMerging(null);
    }
  }

  if (!pr) {
    return (
      // Commit, push, and Create PR all live one tab away — including the gh
      // install and sign-in remediation, which the git card states with the
      // exact commands. Repeating any of that here would be a second copy to
      // keep true; a way over to it is not.
      <div className="flex flex-col items-start gap-3 px-4 py-8">
        <div className="flex flex-col gap-1.5">
          <p className="text-sm font-medium">No pull request yet</p>
          <p className="text-muted-foreground text-xs leading-relaxed">
            Once one exists, its status, checks, and review comments land here.
          </p>
        </div>
        {onOpenSourceControl && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={onOpenSourceControl}
          >
            <GitBranch />
            Open Source control
          </Button>
        )}
      </div>
    );
  }

  const tone = prTone(pr);
  const counts = checkCounts(pr);
  const open = tone === "open" || tone === "draft";
  const branchLine =
    pr.head_branch && pr.base_branch
      ? `${pr.head_branch} → ${pr.base_branch}`
      : (branch ?? null);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-3 py-3">
      {dialog}
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <GitPullRequest
              className={cn("size-4 shrink-0", PR_ICON_TONE_CLASSES[tone])}
            />
            {pr.url ? (
              <a
                href={pr.url}
                className={cn(
                  "text-foreground cursor-pointer truncate rounded-sm text-sm font-semibold underline-offset-2 hover:underline",
                  FOCUS_RING,
                )}
                title={pr.title ?? `#${pr.number}`}
                onClick={(event) => {
                  event.preventDefault();
                  void openExternal(pr.url!).catch(() => undefined);
                }}
              >
                {pr.title ?? `#${pr.number}`}
              </a>
            ) : (
              <p
                className="truncate text-sm font-semibold"
                title={pr.title ?? `#${pr.number}`}
              >
                {pr.title ?? `#${pr.number}`}
              </p>
            )}
          </div>
          {/*
            The base branch is the half a reader checks, and it is the half an
            end-truncate eats first on a long feature-branch name.
          */}
          <MiddleTruncate
            text={branchLine ? `#${pr.number} · ${branchLine}` : `#${pr.number}`}
            className="text-muted-foreground mt-1 font-mono text-xs"
          />
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Badge variant={prStateVariant(tone)} size="sm">
            {prToneLabel(pr)}
          </Badge>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label="Refresh pull request"
            disabled={refreshing}
            onClick={() => void refresh()}
          >
            {refreshing ? <Spinner aria-hidden /> : <RefreshCw />}
          </Button>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-1">
        <ReviewDecisionBadge decision={pr.review_decision} />
        {pr.auto_merge_enabled && (
          <Badge variant="info" size="sm">
            Auto-merge on
          </Badge>
        )}
      </div>

      <CheckList checks={pr.checks ?? []} counts={counts} />

      {open && (
        <div className="flex flex-col gap-2 border-t pt-3">
          <div className="flex items-center gap-2">
            <GitMerge className="text-muted-foreground size-3.5 shrink-0" />
            <Select
              value={method}
              onValueChange={(next) => setMethod(next as CodePrMergeMethod)}
              disabled={merging !== null}
            >
              <SelectTrigger
                className="h-7 flex-1 text-xs"
                aria-label="Merge method"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="squash">Squash and merge</SelectItem>
                <SelectItem value="merge">Merge commit</SelectItem>
                <SelectItem value="rebase">Rebase and merge</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div className="flex items-center gap-2">
            <Button
              type="button"
              size="sm"
              disabled={merging !== null || tone === "draft"}
              onClick={() => void merge(false)}
            >
              {merging === "merge" ? <Spinner aria-hidden /> : null}
              Merge
            </Button>
            {!pr.auto_merge_enabled && (
              <Button
                type="button"
                size="sm"
                variant="secondary"
                disabled={merging !== null || tone === "draft"}
                onClick={() => void merge(true)}
              >
                {merging === "auto" ? <Spinner aria-hidden /> : null}
                Enable auto-merge
              </Button>
            )}
          </div>
          {tone === "draft" && (
            <p className="text-muted-foreground text-xs">
              Mark the pull request ready for review on GitHub to merge it.
            </p>
          )}
          {mergeError && (
            <p className="text-critical text-xs" role="alert">
              {mergeError}
            </p>
          )}
        </div>
      )}

      <CommentsSection
        comments={comments}
        error={commentsError}
        onRetry={() => void loadComments()}
      />
    </div>
  );
}

function ReviewDecisionBadge({ decision }: { decision?: string }) {
  if (!decision) return null;
  if (decision === "approved") {
    return (
      <Badge variant="success" size="sm">
        Approved
      </Badge>
    );
  }
  if (decision === "changes_requested") {
    return (
      <Badge variant="critical" size="sm">
        Changes requested
      </Badge>
    );
  }
  if (decision === "review_required") {
    return (
      <Badge variant="outline" size="sm">
        Review required
      </Badge>
    );
  }
  return null;
}

/** Review conversation, newest last, in the order the server sorted it. */
function CommentsSection({
  comments,
  error,
  onRetry,
}: {
  comments: PullRequestComment[] | null;
  error: string | null;
  onRetry: () => void;
}) {
  return (
    <div className="flex flex-col gap-2 border-t pt-3">
      <div className="text-muted-foreground flex items-center gap-1.5 text-xs font-medium">
        <MessageSquare className="size-3.5" />
        Comments
        {comments && comments.length > 0 && <span>({comments.length})</span>}
      </div>
      {error && (
        <div className="flex flex-col items-start gap-1">
          <p className="text-critical text-xs">{error}</p>
          <Button type="button" variant="outline" size="sm" onClick={onRetry}>
            Retry
          </Button>
        </div>
      )}
      {!error && comments === null && (
        <p className="text-muted-foreground text-xs">Loading…</p>
      )}
      {!error && comments && comments.length === 0 && (
        <p className="text-muted-foreground text-xs">No comments yet.</p>
      )}
      {!error &&
        comments?.map((comment, index) => (
          <CommentRow key={index} comment={comment} />
        ))}
    </div>
  );
}

function CommentRow({ comment }: { comment: PullRequestComment }) {
  const when = comment.created_at
    ? new Date(comment.created_at).toLocaleString(undefined, {
        month: "short",
        day: "numeric",
        hour: "numeric",
        minute: "2-digit",
      })
    : null;
  return (
    <div className="border-border flex flex-col gap-1 rounded-md border px-2 py-1.5">
      <div className="text-muted-foreground flex min-w-0 items-center gap-1.5 text-[11px]">
        <span className="text-foreground shrink-0 font-medium">
          {comment.author ?? "Unknown"}
        </span>
        {comment.kind === "review" && comment.review_state && (
          <span className="shrink-0 capitalize">
            {comment.review_state.replaceAll("_", " ")}
          </span>
        )}
        {comment.kind === "inline" && comment.path && (
          <span className="truncate font-mono" title={comment.path}>
            {comment.path}
            {comment.line !== undefined ? `:${comment.line}` : ""}
          </span>
        )}
        {when && <span className="ml-auto shrink-0 tabular-nums">{when}</span>}
      </div>
      <p className="text-xs leading-relaxed whitespace-pre-wrap">
        {comment.body}
      </p>
    </div>
  );
}

function CheckList({
  checks,
  counts,
}: {
  checks: PullRequestCheck[];
  counts: { passing: number; pending: number; failing: number };
}) {
  const [open, setOpen] = useState(checks.length > 0);
  return (
    <div className="flex flex-col gap-1">
      <button
        type="button"
        className={cn(
          "hover:bg-muted/50 -mx-1 flex cursor-pointer items-center gap-2 rounded-md px-1 py-1 text-left text-xs",
          FOCUS_RING_TIGHT,
          HOVER_TINT,
        )}
        onClick={() => setOpen((current) => !current)}
        aria-expanded={open}
      >
        <CheckCount
          icon={<Check className="size-3.5" />}
          count={counts.passing}
          label="passing"
          // These three carry a word each, so they take the readable ink rather
          // than the mark colour the bare glyphs below use.
          className="text-success-foreground"
        />
        <CheckCount
          icon={<CircleDashed className="size-3.5" />}
          count={counts.pending}
          label="pending"
          className="text-muted-foreground"
        />
        <CheckCount
          icon={<X className="size-3.5" />}
          count={counts.failing}
          label="failing"
          className="text-critical-foreground"
        />
      </button>
      {open &&
        checks.map((check) => (
          <CheckRow key={`${check.name}:${check.detail ?? ""}`} check={check} />
        ))}
    </div>
  );
}

function CheckCount({
  icon,
  count,
  label,
  className,
}: {
  icon: ReactNode;
  count: number;
  label: string;
  className: string;
}) {
  return (
    <span className={cn("flex items-center gap-1 tabular-nums", className)}>
      {icon}
      {count} {label}
    </span>
  );
}

function CheckRow({ check }: { check: PullRequestCheck }) {
  const tone =
    check.bucket === "pass"
      ? "text-success"
      : check.bucket === "fail"
        ? "text-critical"
        : "text-muted-foreground";
  const Icon =
    check.bucket === "pass" ? Check : check.bucket === "fail" ? X : CircleDashed;
  const body = (
    <>
      <Icon className={cn("size-3.5 shrink-0", tone)} />
      <span className="min-w-0 flex-1 truncate" title={check.name}>
        {check.name}
      </span>
      {check.detail && (
        <span className="text-muted-foreground shrink-0 capitalize">
          {check.detail}
        </span>
      )}
    </>
  );
  if (!check.url) {
    return (
      <div className="-mx-1 flex items-center gap-2 px-1 py-1 text-xs">{body}</div>
    );
  }
  return (
    <a
      href={check.url}
      className={cn(
        "hover:bg-muted/50 -mx-1 flex cursor-pointer items-center gap-2 rounded-md px-1 py-1 text-xs",
        FOCUS_RING_TIGHT,
        HOVER_TINT,
      )}
      onClick={(event) => {
        event.preventDefault();
        void openExternal(check.url!).catch(() => undefined);
      }}
    >
      {body}
      <ExternalLink className="text-muted-foreground size-3 shrink-0" />
    </a>
  );
}

function checkCounts(pr: PullRequestDigest): {
  passing: number;
  pending: number;
  failing: number;
} {
  const checks = pr.checks ?? [];
  if (checks.length > 0) {
    return {
      passing: checks.filter((check) => check.bucket === "pass").length,
      pending: checks.filter((check) => check.bucket === "pending").length,
      failing: checks.filter((check) => check.bucket === "fail").length,
    };
  }
  const summary = pr.checks_summary ?? "";
  const passing = Number(/(\d+) passing/.exec(summary)?.[1] ?? 0);
  const pending = Number(/(\d+) pending/.exec(summary)?.[1] ?? 0);
  const failing = Number(/(\d+) failing/.exec(summary)?.[1] ?? 0);
  return { passing, pending, failing };
}

function prStateVariant(
  tone: ReturnType<typeof prTone>,
): "success" | "critical" | "info" | "outline" {
  if (tone === "open") return "success";
  if (tone === "merged") return "info";
  if (tone === "closed") return "critical";
  return "outline";
}
