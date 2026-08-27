import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  Check,
  CircleCheck,
  CircleDashed,
  CircleMinus,
  ExternalLink,
  EyeOff,
  Files,
  GitBranch,
  GitMerge,
  GitPullRequest,
  MessageSquare,
  MessageSquareReply,
  MoreHorizontal,
  RefreshCw,
  RotateCcw,
  X,
} from "lucide-react";
import { toast } from "sonner";

import { HttpError, type ApiClient } from "../api/client";
import type {
  CodePrMergeMethod,
  CodeWorkspacePullRequestFact,
  CodeWorkspaceSnapshot,
  PullRequestCheck,
  PullRequestComment,
  PullRequestDigest,
} from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
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
import { Spinner } from "@/components/ui/spinner";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useConfirm } from "@/components/ConfirmDialog";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { openExternal } from "@/host";
import type { CodeTranscriptItem } from "./CodeSessionReducer";
import { useCodeUiStore } from "./CodeUiStore";
import { DiffOverviewContent, useChangedFilesResource } from "./DiffOverview";
import { FilesPanel } from "./FilesPanel";
import { FOCUS_RING, FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";
import { PrCommentCard } from "./PrCommentCard";
import {
  prDirectMergeAction,
  prMergeControls,
  prWorkflowStatus,
} from "./prActions";
import { useWorkspaceDigest } from "./CodeUpdatesStore";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";
import { useWorkspacePullRequests } from "./useWorkspacePullRequests";
import { digestFromFact, factKey, WorkspacePrList } from "./WorkspacePrList";
import { STATUS_MARK } from "./statusTone";
import {
  PULL_REQUEST_LIFECYCLE_TONE,
  STATUS_TONE_BADGE_VARIANT,
  checkCounts,
  mergeBlockedReasons,
  prStateChips,
  pullRequestLifecycle,
} from "./prState";

export type InspectorTab = "files" | "source" | "pr";

const TAB_TRIGGER_CLASS =
  "text-muted-foreground hover:bg-background/70 hover:text-foreground flex h-8 items-center gap-1.5 rounded-lg px-2.5 py-0 text-xs font-medium";

const TAB_TRIGGER_SELECTED_CLASS =
  "bg-background text-foreground hover:bg-background hover:text-foreground data-[state=active]:bg-background data-[state=active]:text-foreground shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_7%,transparent),inset_0_0_0_1px_var(--border-subtle)]";

/**
 * Optional right workspace pane: Files, Source control, and Pull request as
 * clearly labeled tabs.
 *
 * Files is a nested worktree explorer. Changes is a compact PR-oriented
 * changed-file index; individual patches open in the center pane. Review
 * carries the PR's own life: status, checks, and conversation.
 */
export function CodeInspector({
  client,
  workspaceId,
  workspace,
  contentRevision,
  prResource,
  initialTab,
  onOpenFile,
  onOpenDiff,
  onClose,
}: {
  client: ApiClient;
  workspaceId: string;
  workspace: CodeWorkspaceSnapshot | null;
  contentRevision: number;
  prResource?: CodeWorkspacePrResource;
  initialTab?: InspectorTab;
  onOpenFile?: (path: string, line?: number) => void;
  onOpenDiff?: (path: string) => void;
  onClose?: () => void;
}) {
  const digest = useWorkspaceDigest(workspaceId);
  const pr = prResource?.data?.pr ?? digest?.pr_state ?? workspace?.pr;
  const scope = useCodeUiStore((state) => state.inspectorScope);
  const setInspectorScope = useCodeUiStore((state) => state.setInspectorScope);
  const filesSearchPending = useCodeUiStore(
    (state) => state.filesSearchPending,
  );
  const [tab, setTab] = useState<InspectorTab>(
    initialTab ?? (scope ? "source" : "files"),
  );
  const [file, setFile] = useState<string | undefined>();
  const turnId = scope?.turnId;
  const changedFiles = useChangedFilesResource({
    client,
    workspaceId,
    turnId,
    contentRevision,
  });

  useEffect(() => {
    if (!scope) return;
    setTab("source");
    setFile(undefined);
  }, [scope]);

  // Search lives on the Files tab, so the find chord has to land there first.
  // The panel itself clears the flag once it has the caret; this only moves
  // the tab, which is a no-op when Files is already up.
  useEffect(() => {
    if (filesSearchPending) setTab("files");
  }, [filesSearchPending]);

  useEffect(() => {
    if (!pr && tab === "pr") setTab("source");
  }, [pr, tab]);

  function openFile(next: string, line?: number) {
    if (onOpenFile) {
      onOpenFile(next, line);
      return;
    }
    setFile(next);
    setTab("source");
  }

  function openDiff(next: string) {
    setFile(next);
    if (onOpenDiff) {
      onOpenDiff(next);
      return;
    }
    setTab("source");
  }

  return (
    <aside
      className="flex h-full min-h-0 w-full min-w-0 flex-col overflow-hidden bg-page-background/45"
      aria-label="Workspace surfaces"
      data-testid="code-inspector"
    >
      <Tabs
        value={tab}
        onValueChange={(next) => setTab(next as InspectorTab)}
        className="flex min-h-0 flex-1 flex-col overflow-hidden"
      >
        <header className="flex h-11 shrink-0 items-center gap-1 border-b border-border-subtle bg-page-background/55 px-2">
          <TabsList className="h-auto justify-start gap-0.5 bg-transparent p-0">
            <InspectorTabTrigger
              value="files"
              label="Files"
              displayLabel="Files"
              selected={tab === "files"}
            >
              <Files className="size-3.5" />
            </InspectorTabTrigger>
            <InspectorTabTrigger
              value="source"
              label="Source control"
              displayLabel="Changes"
              selected={tab === "source"}
              count={changedFiles.data?.stat.files}
            >
              <GitBranch className="size-3.5" />
            </InspectorTabTrigger>
            {pr && (
              <InspectorTabTrigger
                value="pr"
                label="Pull request"
                displayLabel="Review"
                selected={tab === "pr"}
              >
                <GitPullRequest
                  className={cn(
                    "size-3.5",
                    STATUS_MARK[
                      PULL_REQUEST_LIFECYCLE_TONE[pullRequestLifecycle(pr)]
                    ],
                  )}
                />
              </InspectorTabTrigger>
            )}
          </TabsList>
          <div className="ml-auto flex min-w-0 items-center gap-1">
            {scope && (
              <button
                type="button"
                className={cn(
                  "text-muted-foreground hover:bg-background hover:text-foreground cursor-pointer truncate rounded-lg border border-border-subtle bg-background/60 px-2 py-1 font-mono text-xs",
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
            {onClose && (
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="rounded-lg"
                aria-label="Close review sidebar"
                onClick={onClose}
              >
                <X />
              </Button>
            )}
          </div>
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
            <DiffOverviewContent
              resource={changedFiles}
              turnId={turnId}
              turnLabel={scope?.label}
              selected={file}
              onOpenFile={openDiff}
            />
          </div>
        </TabsContent>
        <TabsContent
          value="pr"
          className="mt-0 flex min-h-0 flex-1 flex-col overflow-hidden"
        >
          <WorkspacePrTab
            client={client}
            workspaceId={workspaceId}
            pr={pr}
            branch={workspace?.branch_name}
            prResource={prResource}
            prCount={digest?.pr_count}
          />
        </TabsContent>
      </Tabs>
    </aside>
  );
}

function InspectorTabTrigger({
  value,
  label,
  displayLabel,
  selected,
  count,
  children,
}: {
  value: InspectorTab;
  label: string;
  displayLabel: string;
  selected: boolean;
  count?: number;
  children: ReactNode;
}) {
  return (
    <TabsTrigger
      value={value}
      aria-label={label}
      className={cn(TAB_TRIGGER_CLASS, selected && TAB_TRIGGER_SELECTED_CLASS)}
    >
      {children}
      <span>{displayLabel}</span>
      {count !== undefined && (
        <Badge
          variant="secondary"
          size="sm"
          className="h-4 min-w-4 justify-center px-1 font-mono text-2xs font-medium tabular-nums"
          aria-label={`${count} changed ${count === 1 ? "file" : "files"}`}
        >
          {count}
        </Badge>
      )}
    </TabsTrigger>
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
 * The inspector's Pull request tab: the workspace's attributed set above the
 * single-PR panel (decision 62). With one or no attributed pull requests the
 * list stays hidden and this is exactly the live panel it always was.
 * Selecting a non-primary row shows its stored snapshot; the primary row
 * returns to the live resource.
 *
 * Numbers repeat across repositories and the live digest carries no
 * repository identity, so the primary is resolved only when exactly one
 * attributed fact carries the live number. A colliding row is never treated
 * as the primary: selecting it shows its own stored snapshot.
 */
function WorkspacePrTab({
  client,
  workspaceId,
  pr,
  branch,
  prResource,
  prCount,
}: {
  client: ApiClient;
  workspaceId: string;
  pr?: PullRequestDigest;
  branch?: string;
  prResource?: CodeWorkspacePrResource;
  prCount?: number;
}) {
  const attributed = useWorkspacePullRequests(client, workspaceId, prCount);
  const [selected, setSelected] = useState<CodeWorkspacePullRequestFact | null>(
    null,
  );
  useEffect(() => {
    setSelected(null);
  }, [workspaceId]);
  const items = attributed.data?.items ?? [];
  const primaryMatches =
    pr === undefined ? [] : items.filter((item) => item.number === pr.number);
  const primary = primaryMatches.length === 1 ? primaryMatches[0] : undefined;
  const selectedKey = selected
    ? factKey(selected)
    : primary
      ? factKey(primary)
      : undefined;
  const shownPr = selected ? digestFromFact(selected) : pr;
  return (
    <>
      {items.length > 1 && (
        <WorkspacePrList
          items={items}
          selectedKey={selectedKey}
          onSelect={(item) =>
            setSelected(
              primary && factKey(item) === factKey(primary) ? null : item,
            )
          }
        />
      )}
      <PrTab
        client={client}
        workspaceId={workspaceId}
        pr={shownPr}
        branch={branch}
        prResource={selected ? undefined : prResource}
      />
    </>
  );
}

/**
 * The pull request's own life: status, checks, and review comments.
 *
 * Exported so the center can host it as a peer tab; the inspector remains
 * its other caller.
 */
export function PrTab({
  client,
  workspaceId,
  pr,
  branch,
  prResource,
}: {
  client: ApiClient;
  workspaceId: string;
  pr?: PullRequestDigest;
  branch?: string;
  prResource?: CodeWorkspacePrResource;
}) {
  const { confirm, dialog } = useConfirm();
  const [localRefreshing, setLocalRefreshing] = useState(false);
  const [localMerging, setLocalMerging] = useState<
    "merge" | "auto_merge" | null
  >(null);
  const [method, setMethod] = useState<CodePrMergeMethod>("squash");
  const [mergeError, setMergeError] = useState<string | null>(null);
  const [comments, setComments] = useState<PullRequestComment[] | null>(null);
  const [commentsError, setCommentsError] = useState<string | null>(null);
  const prNumber = pr?.number;
  const commentPreferenceKey = `${workspaceId}:${prNumber ?? "none"}`;
  const [commentPreferences, setCommentPreferences] =
    useState<ReviewCommentPreferences>(() =>
      readReviewCommentPreferences(commentPreferenceKey),
    );
  const offerComposerPrompt = useCodeUiStore(
    (state) => state.offerComposerPrompt,
  );
  const sharedMerging =
    prResource?.busy === "merge" || prResource?.busy === "auto_merge"
      ? prResource.busy
      : null;
  const merging = sharedMerging ?? localMerging;
  const refreshing = prResource?.busy === "refresh" || localRefreshing;
  const mutationBusy =
    (prResource ? prResource.busy !== null || prResource.refreshing : false) ||
    localMerging !== null;

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

  useEffect(() => {
    setCommentPreferences(readReviewCommentPreferences(commentPreferenceKey));
  }, [commentPreferenceKey]);

  function updateCommentPreferences(
    update: (current: ReviewCommentPreferences) => ReviewCommentPreferences,
  ) {
    setCommentPreferences((current) => {
      const next = update(current);
      storeReviewCommentPreferences(commentPreferenceKey, next);
      return next;
    });
  }

  function attachComment(comment: PullRequestComment) {
    offerComposerPrompt(workspaceId, commentChatContext(comment));
    toast.success("Review comment attached to chat");
  }

  async function refresh() {
    if (!prResource) setLocalRefreshing(true);
    try {
      const commentsRequest =
        prNumber === undefined ? Promise.resolve() : loadComments();
      const next = await (prResource
        ? prResource.refreshFromHost()
        : client.refreshCodeWorkspacePr(workspaceId));
      if (!next) return;
      await commentsRequest;
    } catch (err) {
      toast.error(
        friendlyErrorMessage(err, "Could not refresh the pull request"),
      );
    } finally {
      if (!prResource) setLocalRefreshing(false);
    }
  }

  async function merge(auto: boolean) {
    if (!pr) return;
    const action = prDirectMergeAction(pr);
    if (!action || action.auto !== auto) return;
    if (!auto) {
      const ok = await confirm({
        title: `Merge #${pr.number}?`,
        description: `The pull request is ${method === "squash" ? "squash-merged" : method === "rebase" ? "rebased and merged" : "merged"} into ${pr.base_branch ?? "its base branch"} on GitHub.`,
        confirmLabel: "Merge",
      });
      if (!ok) return;
    }
    const mutation = auto ? "auto_merge" : "merge";
    if (!prResource) setLocalMerging(mutation);
    setMergeError(null);
    try {
      const next = prResource
        ? await prResource.runMutation(mutation, () =>
            client.mergeCodePr(workspaceId, { method, auto }),
          )
        : await client.mergeCodePr(workspaceId, { method, auto });
      if (!next) return;
      prResource?.adopt(next);
      toast.success(auto ? "Auto-merge enabled" : "Merged");
    } catch (err) {
      if (err instanceof HttpError && err.kind === "pr_not_mergeable") {
        setMergeError(err.message);
      } else {
        toast.error(friendlyErrorMessage(err, "Could not merge"));
      }
    } finally {
      if (!prResource) setLocalMerging(null);
    }
  }

  if (!pr) {
    return (
      <div className="flex flex-col items-start gap-3 px-4 py-8">
        <div className="flex flex-col gap-1.5">
          <p className="text-sm font-medium">No pull request yet</p>
          <p className="text-muted-foreground text-xs leading-relaxed">
            Create one from the workspace header. Its checks and review
            conversation will appear here.
          </p>
        </div>
      </div>
    );
  }

  const lifecycle = pullRequestLifecycle(pr);
  const workflow = prWorkflowStatus(pr);
  const mergeControls = prMergeControls(workflow.state);
  const directMerge = prDirectMergeAction(pr);
  const showMergeMethod = directMerge !== null;
  const counts = checkCounts(pr);
  const blockers = mergeBlockedReasons(pr);
  const open = lifecycle === "open" || lifecycle === "draft";
  const chips = prStateChips(pr);
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
              className={cn(
                "size-4 shrink-0",
                STATUS_MARK[PULL_REQUEST_LIFECYCLE_TONE[lifecycle]],
              )}
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
            text={
              branchLine ? `#${pr.number} · ${branchLine}` : `#${pr.number}`
            }
            className="text-muted-foreground mt-1 font-mono text-xs"
          />
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {chips.map((chip) => (
            <Badge
              key={chip.key}
              variant={STATUS_TONE_BADGE_VARIANT[chip.tone]}
              size="sm"
            >
              {chip.label}
            </Badge>
          ))}
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label="Refresh pull request"
            disabled={
              refreshing ||
              (prResource
                ? prResource.busy !== null || prResource.refreshing
                : false)
            }
            onClick={() => void refresh()}
          >
            {refreshing ? <Spinner aria-hidden /> : <RefreshCw />}
          </Button>
        </div>
      </div>

      <CheckList checks={pr.checks ?? []} counts={counts} />

      {open && (
        <div className="border-border-subtle flex flex-col gap-2.5 rounded-xl border bg-background/45 p-2.5">
          <div className="flex min-w-0 items-start gap-2">
            <span className="mt-0.5 grid size-7 shrink-0 place-items-center text-muted-foreground">
              <GitMerge className="size-3.5" />
            </span>
            <div className="min-w-0 flex-1">
              <p className="text-xs font-medium">Merge pull request</p>
              {blockers.length > 0 ? (
                <ul className="mt-0.5 flex flex-col gap-0.5">
                  {blockers.map((reason) => (
                    <li
                      key={reason}
                      className="text-muted-foreground text-xs leading-4"
                    >
                      {reason}
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="text-muted-foreground mt-0.5 text-xs leading-4">
                  {mergeControls.explanation ??
                    `Ready to land on ${pr.base_branch ?? "the base branch"}.`}
                </p>
              )}
            </div>
            {showMergeMethod && (
              <Select
                value={method}
                onValueChange={(next) => setMethod(next as CodePrMergeMethod)}
                disabled={mutationBusy}
              >
                <SelectTrigger
                  className="h-7 w-[116px] shrink-0 text-xs"
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
            )}
          </div>
          {directMerge?.kind === "merge" ? (
            <Button
              type="button"
              size="sm"
              className="w-full"
              disabled={mutationBusy}
              onClick={() => void merge(false)}
            >
              {merging === "merge" ? <Spinner aria-hidden /> : null}
              {mergeMethodLabel(method)}
            </Button>
          ) : directMerge ? (
            <Button
              type="button"
              size="sm"
              variant="secondary"
              className="w-full"
              disabled={mutationBusy}
              onClick={() => void merge(true)}
            >
              {merging === "auto_merge" ? <Spinner aria-hidden /> : null}
              {directMerge.label}
            </Button>
          ) : workflow.state === "queued" ? (
            <div className="text-info-foreground flex items-center gap-1.5 px-1 text-xs font-medium">
              <CircleDashed className="size-3.5" />
              In merge queue
            </div>
          ) : pr.auto_merge_enabled ? (
            <div className="text-success-foreground flex items-center gap-1.5 px-1 text-xs font-medium">
              <CircleCheck className="size-3.5" />
              Auto-merge is enabled
            </div>
          ) : null}
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
        preferences={commentPreferences}
        onRetry={() => void loadComments()}
        onAttach={attachComment}
        onHide={(key) =>
          updateCommentPreferences((current) => ({
            ...current,
            hidden: addPreference(current.hidden, key),
          }))
        }
        onToggleResolved={(key) =>
          updateCommentPreferences((current) => ({
            ...current,
            resolved: togglePreference(current.resolved, key),
          }))
        }
        onRestoreHidden={() =>
          updateCommentPreferences((current) => ({ ...current, hidden: [] }))
        }
      />
    </div>
  );
}

function mergeMethodLabel(method: CodePrMergeMethod): string {
  switch (method) {
    case "squash":
      return "Squash and merge";
    case "merge":
      return "Create merge commit";
    case "rebase":
      return "Rebase and merge";
  }
}

/** Review conversation, newest last, in the order the server sorted it. */
function CommentsSection({
  comments,
  error,
  preferences,
  onRetry,
  onAttach,
  onHide,
  onToggleResolved,
  onRestoreHidden,
}: {
  comments: PullRequestComment[] | null;
  error: string | null;
  preferences: ReviewCommentPreferences;
  onRetry: () => void;
  onAttach: (comment: PullRequestComment) => void;
  onHide: (key: string) => void;
  onToggleResolved: (key: string) => void;
  onRestoreHidden: () => void;
}) {
  const keyed = (comments ?? []).map((comment) => ({
    comment,
    key: reviewCommentKey(comment),
  }));
  const visible = keyed.filter(({ key }) => !preferences.hidden.includes(key));
  const hiddenCount = keyed.length - visible.length;

  return (
    <div className="flex flex-col gap-2.5 border-t border-border-subtle pt-3">
      <div className="flex items-center gap-1.5">
        <div className="text-muted-foreground flex min-w-0 flex-1 items-center gap-1.5 text-xs font-medium">
          <MessageSquare className="size-3.5" />
          Comments
          {comments && comments.length > 0 && <span>({comments.length})</span>}
        </div>
        {hiddenCount > 0 && (
          <button
            type="button"
            className={cn(
              "text-muted-foreground hover:text-foreground cursor-pointer rounded-sm text-xs",
              FOCUS_RING,
              HOVER_TINT,
            )}
            onClick={onRestoreHidden}
          >
            Show {hiddenCount} hidden
          </button>
        )}
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
      {!error && comments && comments.length > 0 && visible.length === 0 && (
        <p className="text-muted-foreground text-xs">
          All comments are hidden in Tidebreak.
        </p>
      )}
      {!error &&
        visible.map(({ comment, key }) => (
          <CommentRow
            key={key}
            comment={comment}
            resolved={preferences.resolved.includes(key)}
            onAttach={() => onAttach(comment)}
            onHide={() => onHide(key)}
            onToggleResolved={() => onToggleResolved(key)}
          />
        ))}
    </div>
  );
}

function CommentRow({
  comment,
  resolved,
  onAttach,
  onHide,
  onToggleResolved,
}: {
  comment: PullRequestComment;
  resolved: boolean;
  onAttach: () => void;
  onHide: () => void;
  onToggleResolved: () => void;
}) {
  const author = comment.author ?? "Unknown";
  return (
    <PrCommentCard
      comment={comment}
      resolved={resolved}
      actions={
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className="-mr-1 -mt-1 rounded-lg opacity-70 group-hover/comment:opacity-100 data-[state=open]:opacity-100"
              aria-label={`Comment actions for ${author}`}
            >
              <MoreHorizontal />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-52">
            <DropdownMenuItem onSelect={onAttach}>
              <MessageSquareReply />
              Attach to chat
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={onToggleResolved}>
              {resolved ? <RotateCcw /> : <CircleCheck />}
              {resolved
                ? "Mark unresolved in Tidebreak"
                : "Mark resolved in Tidebreak"}
            </DropdownMenuItem>
            {comment.url && (
              <DropdownMenuItem
                onSelect={() =>
                  void openExternal(comment.url!).catch(() => undefined)
                }
              >
                <ExternalLink />
                Open on GitHub
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={onHide}>
              <EyeOff />
              Hide in Tidebreak
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      }
    />
  );
}

type ReviewCommentPreferences = {
  hidden: string[];
  resolved: string[];
};

const REVIEW_COMMENT_PREFS_KEY = "tidebreak.review-comment-prefs.v1";

function reviewCommentKey(comment: PullRequestComment): string {
  if (comment.id) return `${comment.kind}:${comment.id}`;
  const identity = [
    comment.kind,
    comment.author ?? "",
    comment.created_at ?? "",
    comment.path ?? "",
    comment.line ?? "",
    comment.body,
  ].join("\u0000");
  let hash = 2166136261;
  for (let index = 0; index < identity.length; index += 1) {
    hash ^= identity.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${comment.kind}:local-${(hash >>> 0).toString(36)}`;
}

function readReviewCommentPreferences(key: string): ReviewCommentPreferences {
  if (typeof window === "undefined") return { hidden: [], resolved: [] };
  try {
    const raw = window.localStorage.getItem(REVIEW_COMMENT_PREFS_KEY);
    if (!raw) return { hidden: [], resolved: [] };
    const all = JSON.parse(raw) as Record<string, unknown>;
    const value = all[key];
    if (!value || typeof value !== "object") {
      return { hidden: [], resolved: [] };
    }
    const preferences = value as Partial<ReviewCommentPreferences>;
    return {
      hidden: Array.isArray(preferences.hidden)
        ? preferences.hidden.filter(
            (item): item is string => typeof item === "string",
          )
        : [],
      resolved: Array.isArray(preferences.resolved)
        ? preferences.resolved.filter(
            (item): item is string => typeof item === "string",
          )
        : [],
    };
  } catch {
    return { hidden: [], resolved: [] };
  }
}

function storeReviewCommentPreferences(
  key: string,
  preferences: ReviewCommentPreferences,
) {
  if (typeof window === "undefined") return;
  try {
    const raw = window.localStorage.getItem(REVIEW_COMMENT_PREFS_KEY);
    const all = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
    all[key] = preferences;
    window.localStorage.setItem(REVIEW_COMMENT_PREFS_KEY, JSON.stringify(all));
  } catch {
    // Comment display preferences are best-effort and never block review.
  }
}

function addPreference(values: readonly string[], value: string): string[] {
  return values.includes(value) ? [...values] : [...values, value];
}

function togglePreference(values: readonly string[], value: string): string[] {
  return values.includes(value)
    ? values.filter((item) => item !== value)
    : [...values, value];
}

function commentChatContext(comment: PullRequestComment): string {
  const author = comment.author ? `@${comment.author}` : "a reviewer";
  const anchor = comment.path
    ? ` on \`${comment.path}${comment.line !== undefined ? `:${comment.line}` : ""}\``
    : "";
  const quote = comment.body
    .split("\n")
    .map((line) => `> ${line}`)
    .join("\n");
  return `Review comment from ${author}${anchor}:\n\n${quote}\n\nHelp me address this feedback.`;
}

function CheckList({
  checks,
  counts,
}: {
  checks: PullRequestCheck[];
  counts: {
    passing: number;
    pending: number;
    failing: number;
    skipped: number;
  };
}) {
  const [open, setOpen] = useState(checks.length > 0);
  return (
    <div className="flex flex-col gap-1">
      <button
        type="button"
        className={cn(
          "hover:bg-muted/50 -mx-1 flex cursor-pointer flex-wrap items-center gap-x-2 gap-y-1 rounded-md px-1 py-1 text-left text-xs",
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
          // These counts carry words, so they take the readable ink rather
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
        {counts.skipped > 0 && (
          <CheckCount
            icon={<CircleMinus className="size-3.5" />}
            count={counts.skipped}
            label="skipped"
            className="text-muted-foreground"
          />
        )}
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
    check.bucket === "pass"
      ? Check
      : check.bucket === "fail"
        ? X
        : check.bucket === "skipped"
          ? CircleMinus
          : CircleDashed;
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
      <div className="-mx-1 flex items-center gap-2 px-1 py-1 text-xs">
        {body}
      </div>
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
