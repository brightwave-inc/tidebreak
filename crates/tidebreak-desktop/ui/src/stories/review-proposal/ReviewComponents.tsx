import { useId, useState, type ReactNode } from "react";
import {
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  Check,
  ChevronDown,
  CircleAlert,
  CircleCheck,
  Clock3,
  FileCode2,
  GitBranch,
  GitPullRequest,
  MessageSquare,
  RefreshCw,
  X,
} from "lucide-react";
import type {
  CodeDeliveryCheck,
  CodeDeliveryPullRequestFile,
  CodeDeliveryPullRequestSummary,
} from "@/api/types";
import {
  checkCounts,
  checkSummary,
  mergeBlockedReasons,
  prStateChips,
  prStatus,
  STATUS_TONE_BADGE_VARIANT,
} from "@/code/prState";
import { STATUS_TEXT, type StatusTone } from "@/code/statusTone";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";

export type LoadState = "ready" | "loading" | "refreshing" | "failed";
export function RefreshNotice({
  state,
  onRetry,
}: {
  state: LoadState;
  onRetry: () => void;
}) {
  if (state === "failed")
    return (
      <div
        role="alert"
        className="notice-surface notice-warning mx-5 my-3 flex flex-wrap items-center justify-between gap-2 p-3 text-sm"
      >
        <span>
          Could not refresh GitHub. Your last results are still available.
        </span>
        <Button size="sm" variant="outline" onClick={onRetry}>
          Retry refresh
        </Button>
      </div>
    );
  return (
    <div
      role="status"
      className="flex items-center gap-2 text-xs text-muted-foreground"
    >
      {state === "refreshing" ? (
        <>
          <Spinner className="size-3" aria-hidden />
          Refreshing GitHub…
        </>
      ) : (
        "Last confirmed 20 seconds ago"
      )}
    </div>
  );
}

export function ReviewHeader({
  pr,
  onBack,
  onInspect,
  onRefresh,
  onMerge,
  refreshing = false,
}: {
  pr: CodeDeliveryPullRequestSummary;
  onBack: () => void;
  onInspect: () => void;
  onRefresh: () => void;
  onMerge: () => void;
  refreshing?: boolean;
}) {
  const status = prStatus(pr);
  const blockers = mergeBlockedReasons(pr);
  return (
    <header className="px-5 pt-4">
      <div className="mb-3 flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
        <Button size="sm" variant="ghost" className="-ml-2" onClick={onBack}>
          <ArrowLeft />
          Pull requests
        </Button>
        <span>
          {pr.repository.name} #{pr.number}
        </span>
      </div>
      <div className="review-proposal-heading flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-xl font-semibold tracking-tight wrap-anywhere">
            {pr.title}
          </h1>
          <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            {prStateChips(pr)
              .slice(0, 1)
              .map((chip) => (
                <Badge
                  key={chip.key}
                  size="sm"
                  variant={STATUS_TONE_BADGE_VARIANT[chip.tone]}
                >
                  {chip.label}
                </Badge>
              ))}
            <span>{pr.author}</span>
            <span className="flex min-w-0 items-start gap-1 font-mono">
              <GitBranch className="mt-0.5 size-3 shrink-0" />
              <span className="min-w-0 wrap-anywhere">
                {pr.head_branch} <ArrowRight className="inline size-3" />{" "}
                {pr.base_branch}
              </span>
            </span>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button
            size="sm"
            onClick={status.gate === "ready" ? onMerge : onInspect}
          >
            {status.gate === "ready"
              ? "Review merge…"
              : status.checks.failing
                ? "Inspect failure"
                : "Review feedback"}
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label="Refresh pull request"
            onClick={onRefresh}
            disabled={refreshing}
          >
            {refreshing ? <Spinner aria-hidden /> : <RefreshCw />}
          </Button>
        </div>
      </div>
      <div className="mt-4 border-t border-border-subtle py-3">
        <div
          className={cn(
            "flex flex-wrap items-center gap-2 text-sm",
            STATUS_TEXT[status.headline.tone],
          )}
        >
          {status.gate === "ready" ? (
            <CircleCheck className="size-3.5" />
          ) : (
            <CircleAlert className="size-3.5" />
          )}
          <span className="font-medium">{status.headline.label}</span>
          {status.checks.failing > 0 && (
            <span>· {checkSummary(status.checks).label}</span>
          )}
        </div>
        {blockers.length > 0 && (
          <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
            {blockers.join(" ")}
          </p>
        )}
      </div>
    </header>
  );
}

export function PullRequestRow({
  pr,
  onOpen,
}: {
  pr: CodeDeliveryPullRequestSummary;
  onOpen: () => void;
}) {
  const status = prStatus(pr);
  return (
    <button
      type="button"
      onClick={onOpen}
      className="review-proposal-pr-row grid w-full grid-cols-[minmax(0,1fr)_190px] items-center gap-5 border-b border-border-subtle px-5 py-4 text-left hover:bg-muted/30 focus-visible:outline-2 focus-visible:outline-ring"
      aria-label={`Open PR ${pr.number}: ${pr.title}`}
    >
      <div className="min-w-0">
        <div className="flex items-start gap-2">
          <GitPullRequest
            className={cn(
              "mt-0.5 size-4 shrink-0",
              STATUS_TEXT[status.headline.tone],
            )}
          />
          <span className="text-md font-medium wrap-anywhere">{pr.title}</span>
        </div>
        <p className="mt-1 pl-6 text-xs text-muted-foreground">
          {pr.repository.name} #{pr.number} · {pr.author}
        </p>
      </div>
      <div className="text-sm">
        <div className={STATUS_TEXT[status.headline.tone]}>
          {status.headline.label}
        </div>
        <p className="mt-1 text-xs text-muted-foreground">
          {checkSummary(checkCounts(pr)).label} · {pr.comment_count} comments
        </p>
      </div>
    </button>
  );
}

export function ReviewEmpty({
  title = "No pull requests need your attention",
  description = "You are caught up. Open All to see the rest of your pull requests.",
}: {
  title?: string;
  description?: string;
}) {
  return (
    <Empty className="min-h-64">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <GitPullRequest />
        </EmptyMedia>
        <EmptyTitle>{title}</EmptyTitle>
        <EmptyDescription>{description}</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}
export function ReviewLoading({
  label = "Loading pull requests…",
}: {
  label?: string;
}) {
  return (
    <div className="space-y-6 p-5" role="status" aria-label={label}>
      <span className="flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner aria-hidden />
        {label}
      </span>
      {[0, 1, 2].map((n) => (
        <div key={n} className="space-y-2">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-3 w-1/3" />
        </div>
      ))}
    </div>
  );
}

type PatchLine = {
  oldLine?: number;
  newLine?: number;
  text: string;
  kind: "add" | "remove" | "context" | "hunk";
};
function patchLines(patch: string): PatchLine[] {
  let oldLine = 0;
  let newLine = 0;
  return patch.split("\n").map((text) => {
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(text);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      return { text, kind: "hunk" };
    }
    if (text.startsWith("+"))
      return { text: text.slice(1), kind: "add", newLine: newLine++ };
    if (text.startsWith("-"))
      return { text: text.slice(1), kind: "remove", oldLine: oldLine++ };
    if (text.startsWith("\\")) return { text, kind: "context" };
    return {
      text: text.slice(1),
      kind: "context",
      oldLine: oldLine++,
      newLine: newLine++,
    };
  });
}
export function FileDiff({
  file,
  actions,
  wrap = true,
}: {
  file: CodeDeliveryPullRequestFile;
  actions?: ReactNode;
  wrap?: boolean;
}) {
  return (
    <section className="min-w-0" aria-label={`Diff for ${file.path}`}>
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <span className="min-w-0 font-mono text-sm wrap-anywhere">
          {file.path.split("/").at(-1)}{" "}
          <span className={STATUS_TEXT.ready}>+{file.additions}</span>{" "}
          <span className={STATUS_TEXT.critical}>−{file.deletions}</span>
        </span>
        <div className="flex flex-wrap items-center gap-2">{actions}</div>
      </div>
      <div className="overflow-x-auto rounded-lg border border-border-subtle font-mono text-sm">
        {file.patch ? (
          patchLines(file.patch).map((line, n) =>
            line.kind === "hunk" ? (
              <div
                key={n}
                className="bg-muted/50 px-3 py-1.5 text-xs text-muted-foreground whitespace-pre-wrap wrap-anywhere"
              >
                {line.text}
              </div>
            ) : (
              <div
                key={n}
                className={cn(
                  "grid min-h-6 grid-cols-[2.5rem_2.5rem_1rem_minmax(0,1fr)]",
                  line.kind === "add" && "bg-success/10",
                  line.kind === "remove" && "bg-critical/10",
                  !wrap && "w-max min-w-full",
                )}
              >
                <span className="py-0.5 pr-2 text-right text-xs text-muted-foreground select-none">
                  {line.oldLine}
                </span>
                <span className="py-0.5 pr-2 text-right text-xs text-muted-foreground select-none">
                  {line.newLine}
                </span>
                <span
                  aria-label={
                    line.kind === "add"
                      ? "Added"
                      : line.kind === "remove"
                        ? "Removed"
                        : undefined
                  }
                  className={cn(
                    "py-0.5",
                    line.kind === "add" && STATUS_TEXT.ready,
                    line.kind === "remove" && STATUS_TEXT.critical,
                  )}
                >
                  {line.kind === "add"
                    ? "+"
                    : line.kind === "remove"
                      ? "−"
                      : " "}
                </span>
                <code
                  className={cn(
                    "py-0.5 pr-3",
                    wrap
                      ? "whitespace-pre-wrap wrap-anywhere"
                      : "whitespace-pre",
                  )}
                >
                  {line.text || " "}
                </code>
              </div>
            ),
          )
        ) : (
          <p className="p-4 text-sm text-muted-foreground">
            No text diff is available for this file.
          </p>
        )}
      </div>
    </section>
  );
}

export function FileRail({
  files,
  selected,
  onSelect,
  viewed = [],
}: {
  files: CodeDeliveryPullRequestFile[];
  selected: string;
  onSelect: (path: string) => void;
  viewed?: string[];
}) {
  return (
    <>
      <aside
        className="review-proposal-file-rail border-r border-border-subtle bg-muted/20 px-2 py-4"
        aria-label="Changed files"
      >
        <h2 className="px-2 pb-3 text-xs font-medium text-muted-foreground">
          {files.length} {files.length === 1 ? "file" : "files"} changed
        </h2>
        {files.map((file) => (
          <button
            type="button"
            key={file.path}
            aria-pressed={selected === file.path}
            onClick={() => onSelect(file.path)}
            className={cn(
              "mb-1 flex w-full items-start gap-2 rounded-md px-2 py-2.5 text-left text-sm hover:bg-muted/50 focus-visible:outline-2 focus-visible:outline-ring",
              selected === file.path && "bg-muted",
            )}
          >
            {viewed.includes(file.path) ? (
              <Check className="mt-0.5 size-3.5 shrink-0" />
            ) : (
              <FileCode2 className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
            )}
            <span className="min-w-0 wrap-anywhere">
              {file.path.split("/").at(-1)}
              <span className="mt-1 block text-xs text-muted-foreground">
                {file.path.split("/").slice(0, -1).join("/")}
              </span>
            </span>
          </button>
        ))}
      </aside>
      <label className="review-proposal-file-picker hidden px-4 pt-4 text-xs text-muted-foreground">
        Changed file
        <select
          value={selected}
          onChange={(event) => onSelect(event.target.value)}
          className="mt-1 h-control w-full rounded-md border border-input bg-background px-2 font-mono text-sm text-foreground"
        >
          {files.map((file) => (
            <option key={file.path} value={file.path}>
              {file.path.split("/").at(-1)}
            </option>
          ))}
        </select>
      </label>
    </>
  );
}

export function ReviewThread({
  body,
  author = "devon",
  path,
  line = 345,
  initialResolved = false,
  onAgent,
}: {
  body: string;
  author?: string;
  path: string;
  line?: number;
  initialResolved?: boolean;
  onAgent: (context: string) => void;
}) {
  const [resolved, setResolved] = useState(initialResolved);
  const [replyOpen, setReplyOpen] = useState(false);
  const [reply, setReply] = useState("");
  const [replies, setReplies] = useState<string[]>([]);
  const id = useId();
  return (
    <section
      className="mt-4 border-t border-border-subtle pt-4"
      aria-label="Inline review thread"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2 text-sm">
          <MessageSquare className="size-3.5 text-muted-foreground" />
          <span className="font-medium">{author}</span>
          <span className="text-xs text-muted-foreground">Line {line}</span>
        </div>
        <span
          className={cn("text-xs", STATUS_TEXT[resolved ? "ready" : "neutral"])}
        >
          {resolved ? "Resolved" : "Unresolved"}
        </span>
      </div>
      <p className="my-3 text-md leading-relaxed">{body}</p>
      {replies.map((text, i) => (
        <p key={i} className="my-2 border-l-2 border-border pl-3 text-sm">
          <span className="font-medium">You · </span>
          {text}
        </p>
      ))}
      <div className="-ml-2 flex flex-wrap gap-1">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => onAgent(`${path}:${line}\n${body}`)}
        >
          Add to agent message
        </Button>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => setReplyOpen(!replyOpen)}
        >
          Reply
        </Button>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => setResolved(!resolved)}
        >
          {resolved ? "Reopen thread" : "Resolve thread"}
        </Button>
      </div>
      {replyOpen && (
        <form
          className="mt-3 space-y-2"
          onSubmit={(event) => {
            event.preventDefault();
            if (!reply.trim()) return;
            setReplies([...replies, reply.trim()]);
            setReply("");
            setReplyOpen(false);
          }}
        >
          <label htmlFor={id} className="text-xs">
            Reply to {author}
          </label>
          <Textarea
            id={id}
            value={reply}
            onChange={(event) => setReply(event.target.value)}
            rows={2}
          />
          <Button size="sm" disabled={!reply.trim()} type="submit">
            Add sample reply
          </Button>
        </form>
      )}
    </section>
  );
}

export function CheckRun({
  check,
  log,
  defaultOpen = false,
  onAgent,
}: {
  check: CodeDeliveryCheck;
  log?: string;
  defaultOpen?: boolean;
  onAgent: (context: string) => void;
}) {
  const [queued, setQueued] = useState(false);
  const [open, setOpen] = useState(defaultOpen);
  const bucket = queued ? "pending" : check.bucket;
  const tone: StatusTone =
    bucket === "fail" ? "critical" : bucket === "pass" ? "ready" : "pending";
  const Icon =
    bucket === "fail" ? CircleAlert : bucket === "pass" ? CircleCheck : Clock3;
  return (
    <section className="border-b border-border-subtle py-3">
      <div className="flex flex-wrap items-center gap-3">
        <Icon className={cn("size-4 shrink-0", STATUS_TEXT[tone])} />
        <div className="min-w-0 flex-1">
          <h3 className="text-sm font-medium">{check.name}</h3>
          <p className={cn("mt-0.5 text-xs", STATUS_TEXT[tone])}>
            {queued
              ? "Rerun queued"
              : (check.detail ??
                checkSummary(checkCounts({ checks: [check] })).label)}
          </p>
        </div>
        {log && (
          <Button
            size="sm"
            variant="ghost"
            aria-expanded={open}
            onClick={() => setOpen(!open)}
          >
            {open ? "Hide logs" : "Show logs"}
            <ChevronDown className={open ? "rotate-180" : ""} />
          </Button>
        )}
        {check.bucket === "fail" && (
          <Button
            size="sm"
            variant="outline"
            disabled={queued}
            onClick={() => setQueued(true)}
          >
            {queued ? "Queued" : "Rerun failed"}
          </Button>
        )}
      </div>
      {open && log && (
        <>
          <pre className="mt-3 whitespace-pre-wrap rounded-lg bg-muted/40 p-3 font-mono text-xs leading-relaxed wrap-anywhere">
            {log}
          </pre>
          <Button
            size="sm"
            variant="ghost"
            className="mt-2"
            onClick={() => onAgent(`${check.name}\n${log}`)}
          >
            Ask agent to fix
          </Button>
        </>
      )}
    </section>
  );
}

export function CommitControls({
  stagedCount,
  ahead,
  onCommit,
  onPush,
  busy = false,
  blocked = false,
  error,
}: {
  stagedCount: number;
  ahead: number;
  onCommit: (message: string) => void;
  onPush: () => void;
  busy?: boolean;
  blocked?: boolean;
  error?: string;
}) {
  const [message, setMessage] = useState("Keep review drafts during refresh");
  const id = useId();
  return (
    <form
      className="space-y-2 border-t border-border-subtle p-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (message.trim() && stagedCount > 0 && !busy && !blocked)
          onCommit(message.trim());
      }}
    >
      <label htmlFor={id} className="text-xs font-medium">
        Commit message
      </label>
      <div className="review-proposal-commit-row flex gap-2">
        <Input
          id={id}
          value={message}
          onChange={(event) => setMessage(event.target.value)}
          className="min-w-0 flex-1"
        />
        <Button
          size="sm"
          type="submit"
          disabled={stagedCount === 0 || !message.trim() || busy || blocked}
        >
          {busy && <Spinner aria-hidden />}Commit staged ({stagedCount})
        </Button>
        <Button
          size="sm"
          variant="outline"
          type="button"
          disabled={ahead === 0 || busy || blocked}
          onClick={onPush}
        >
          <ArrowUp />
          {blocked
            ? "Push blocked"
            : ahead === 0
              ? "Up to date"
              : `Push ${ahead} ${ahead === 1 ? "commit" : "commits"}`}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        {blocked
          ? "Resolve the conflict before committing or pushing."
          : "Only staged changes enter this commit."}
      </p>
      {error && (
        <p role="alert" className={cn("text-sm", STATUS_TEXT.critical)}>
          {error}
        </p>
      )}
    </form>
  );
}

export function AgentDraft({
  context,
  onClose,
}: {
  context: string;
  onClose: () => void;
}) {
  const [message, setMessage] = useState(
    `Address this review feedback. Leave the changes uncommitted.\n\n${context}`,
  );
  const id = useId();
  return (
    <section
      className="border-t border-border-subtle bg-muted/20 p-4"
      aria-label="Agent message draft"
    >
      <div className="mb-2 flex items-center justify-between gap-2">
        <h2 className="text-sm font-medium">Message to agent</h2>
        <Button
          size="icon-sm"
          variant="ghost"
          onClick={onClose}
          aria-label="Close agent draft"
        >
          <X />
        </Button>
      </div>
      <label htmlFor={id} className="mb-2 block text-xs text-muted-foreground">
        Review context and instruction
      </label>
      <Textarea
        id={id}
        value={message}
        onChange={(event) => setMessage(event.target.value)}
        rows={4}
      />
      <p className="mt-2 text-xs text-muted-foreground">
        Storybook draft only. No agent starts.
      </p>
    </section>
  );
}

export function ViewedControl({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  const id = useId();
  return (
    <label
      htmlFor={id}
      className="flex items-center gap-2 text-xs text-muted-foreground"
    >
      <Checkbox
        id={id}
        checked={checked}
        onCheckedChange={(value) => onChange(value === true)}
      />
      Viewed
    </label>
  );
}
