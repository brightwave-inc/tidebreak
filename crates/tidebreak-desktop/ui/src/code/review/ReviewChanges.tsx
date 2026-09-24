import { useEffect, useId, useMemo, useState } from "react";
import { useRouter } from "@tanstack/react-router";
import { CircleAlert, CircleCheck, ScanSearch, X } from "lucide-react";
import { toast } from "sonner";

import type { ApiClient } from "../../api/client";
import type {
  CodeReviewSnapshot,
  HarnessDoctorEntry,
  HarnessKind,
  ModelInfo,
} from "../../api/types";
import { Loader } from "@/components/motion/loader";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { SegmentedControl } from "@/components/ui/segmented";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { LiveLabel } from "@/LiveLabel";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { HarnessModelMenu } from "../CodeComposer";
import { useCodeCatalogStore } from "../CodeCatalogStore";
import { useCodeUiStore } from "../CodeUiStore";
import { usePendingReview, usePendingReviewStore } from "../diff/pendingReview";
import { HARNESS_ICONS } from "../HarnessPicker";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "../interactive";
import {
  gatewayCodeModels,
  HARNESS_LABELS,
  preferredCodeModels,
  type CodeModelOption,
} from "../labels";
import { formatElapsedDuration } from "../TurnReviewCard";
import { STATUS_MARK, type StatusTone } from "../statusTone";
import { renderWorkflowPrompt } from "../workflowPrompts";
import {
  defaultReviewEngine,
  reviewEngineChoices,
  type ReviewEngineChoice,
} from "./reviewEngines";
import {
  useCodeReviewStore,
  useReviewStopping,
  useWorkspaceReview,
  type ReviewClient,
} from "./reviewStore";

/**
 * Review changes, where a person looks at them: the diff's header opens a
 * short form (engine, model, which changes), and a status line under the
 * header follows the review while it runs and says how it ended. Findings
 * arrive in the diff as comments the person keeps or dismisses.
 *
 * Nothing runs until the person starts it, and the form shows the model the
 * review will run on, so the cost of a pass is never a surprise.
 */

/** Which changes a review covers, as the form offers them. */
export type ReviewScopeChoice = {
  /** `WORKING_TREE` or a turn id. */
  readonly value: string;
  readonly label: string;
};

export const WORKING_TREE = "working-tree";

/** Engine picked last time, so the next review starts there. */
const REMEMBERED_ENGINE_KEY = "tidebreak.code-review-engine";

function rememberedEngine(): HarnessKind | null {
  try {
    const value = window.localStorage.getItem(REMEMBERED_ENGINE_KEY);
    return value ? (value as HarnessKind) : null;
  } catch {
    return null;
  }
}

function rememberEngine(kind: HarnessKind) {
  try {
    window.localStorage.setItem(REMEMBERED_ENGINE_KEY, kind);
  } catch {
    // A preference, nothing more.
  }
}

/** A header action: a quiet icon and word, like "Open file" beside it. */
const HEADER_ACTION =
  "text-muted-foreground hover:bg-muted hover:text-foreground flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-1 text-xs whitespace-nowrap disabled:cursor-not-allowed disabled:opacity-60";

/** The form's fields, without the stores that fill them. */
export function ReviewChangesForm({
  choices,
  author,
  engine,
  onEngineChange,
  modelOptions,
  model,
  modelLoading = false,
  onModelChange,
  scopes,
  scope,
  onScopeChange,
  starting = false,
  error,
  onStart,
  onCancel,
}: {
  choices: readonly ReviewEngineChoice[];
  /** The engine that wrote the changes. */
  author: HarnessKind | null;
  engine: HarnessKind | null;
  onEngineChange: (kind: HarnessKind) => void;
  modelOptions: readonly CodeModelOption[];
  model?: string;
  modelLoading?: boolean;
  onModelChange?: (model: string) => void;
  scopes: readonly ReviewScopeChoice[];
  scope: string;
  onScopeChange: (value: string) => void;
  starting?: boolean;
  error?: string | null;
  onStart: () => void;
  onCancel: () => void;
}) {
  const id = useId();
  const selected = choices.find((choice) => choice.entry.kind === engine);
  const ready = selected !== undefined && selected.reason === null;
  const sameAsAuthor = engine !== null && engine === author;
  const othersReady = choices.some(
    (choice) => choice.reason === null && choice.entry.kind !== author,
  );
  const Icon = engine ? HARNESS_ICONS[engine] : null;
  return (
    <form
      className="flex flex-col gap-3"
      aria-labelledby={`${id}-title`}
      onSubmit={(event) => {
        event.preventDefault();
        if (ready && !starting) onStart();
      }}
    >
      <div className="flex flex-col gap-1">
        <h2 id={`${id}-title`} className="text-md font-medium">
          Review changes
        </h2>
        <p className="text-muted-foreground text-xs">
          Another engine reviews a copy of the changes as they are now. It can
          read what your account can read, as coding engines can, but it
          can&apos;t change your files, and anything it asks to run or change is
          refused. Edits you make after you start aren&apos;t part of the
          review. Its findings arrive in the diff for you to keep or dismiss.
        </p>
      </div>
      <div className="flex flex-col gap-1.5">
        <span id={`${id}-engine`} className="text-xs font-medium">
          Engine
        </span>
        <Select
          value={engine ?? undefined}
          onValueChange={(next) => onEngineChange(next as HarnessKind)}
          disabled={choices.length === 0 || starting}
        >
          <SelectTrigger aria-labelledby={`${id}-engine`}>
            <SelectValue placeholder="No engine can review">
              {engine && Icon && (
                <span className="flex items-center gap-2">
                  <Icon className="size-4 shrink-0" />
                  <span>{HARNESS_LABELS[engine]}</span>
                </span>
              )}
            </SelectValue>
          </SelectTrigger>
          <SelectContent scrollButtons={false}>
            {choices.map(({ entry, reason }) => {
              const RowIcon = HARNESS_ICONS[entry.kind];
              const note =
                reason ??
                (entry.kind === author ? "Wrote these changes" : null);
              return (
                <SelectItem
                  key={entry.kind}
                  value={entry.kind}
                  disabled={reason !== null}
                >
                  <span className="flex items-center gap-2.5">
                    <RowIcon className="size-4 shrink-0" />
                    <span className="flex min-w-0 flex-col text-left">
                      <span className="truncate font-medium">
                        {HARNESS_LABELS[entry.kind]}
                      </span>
                      {note && (
                        <span className="text-muted-foreground truncate text-xs">
                          {note}
                        </span>
                      )}
                    </span>
                  </span>
                </SelectItem>
              );
            })}
          </SelectContent>
        </Select>
        {sameAsAuthor && engine && (
          <p className="text-muted-foreground text-xs">
            {othersReady
              ? `${HARNESS_LABELS[engine]} wrote these changes. Another engine gives a second opinion.`
              : `No other engine is ready, so ${HARNESS_LABELS[engine]} reviews its own changes.`}
          </p>
        )}
        {engine === null && (
          <p className="text-muted-foreground text-xs">
            No engine can review right now. Install or sign in to one in
            Settings, Coding engines.
          </p>
        )}
      </div>
      {engine && (
        <div className="flex flex-col gap-1.5">
          <span className="text-xs font-medium">Model</span>
          <HarnessModelMenu
            harness={engine}
            variant="field"
            options={modelOptions}
            value={model}
            loading={modelLoading}
            onChange={onModelChange}
            disabled={starting}
          />
        </div>
      )}
      {scopes.length > 1 && (
        <div className="flex flex-col gap-1.5">
          <span className="text-xs font-medium">Changes</span>
          <SegmentedControl
            aria-label="Changes to review"
            value={scope}
            onValueChange={onScopeChange}
            options={scopes}
          />
        </div>
      )}
      {error && (
        <p role="alert" className="text-critical text-xs">
          {error}
        </p>
      )}
      <div className="flex items-center justify-end gap-1.5">
        <Button type="button" variant="ghost" size="sm" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" size="sm" disabled={!ready || starting}>
          {starting && <Spinner aria-hidden />}
          Start review
        </Button>
      </div>
    </form>
  );
}

/** What the Review changes control needs from the page that shows the diff. */
export type DiffReviewerContext = {
  client: ReviewClient & Partial<Pick<ApiClient, "listCodeHarnessModels">>;
  /** The conversation the review is started from, and recorded in. */
  sessionId: string;
  /** The engine that wrote the changes: that conversation's. */
  author: HarnessKind | null;
  harnesses: readonly HarnessDoctorEntry[];
  catalogModels?: readonly ModelInfo[];
  defaultModelKey?: string | null;
};

/**
 * The Review changes button in a diff's header, and the form it opens.
 *
 * `turn` is the turn the diff shows, when it shows one: the form then
 * offers that turn's changes first, and the working tree second.
 */
export function ReviewChangesControl({
  workspaceId,
  reviewer,
  turn,
  takesRequests = false,
}: {
  workspaceId: string;
  reviewer: DiffReviewerContext;
  turn?: { id: string; label: string };
  /** Open when a Review changes action elsewhere asks for this workspace. */
  takesRequests?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const review = useCodeReviewStore((state) => state.byWorkspace[workspaceId]);
  const running = review?.status === "running";
  const pending = useCodeUiStore((state) => state.reviewFormPending);
  useEffect(() => {
    if (!takesRequests || pending !== workspaceId) return;
    if (useCodeUiStore.getState().takeReviewForm(workspaceId) && !running) {
      setOpen(true);
    }
  }, [takesRequests, pending, workspaceId, running]);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={cn(
            HEADER_ACTION,
            FOCUS_RING_TIGHT,
            HOVER_TINT,
            // Its words say why it is off, so they stay readable.
            running && "disabled:opacity-100",
          )}
          disabled={running}
        >
          <ScanSearch className="size-3" aria-hidden />
          {/* A disabled button takes no focus and shows no tooltip to a
              keyboard, so why it is disabled is in its own words. */}
          {running ? "Review running" : "Review changes"}
        </button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        sideOffset={6}
        aria-label="Review changes"
        className="w-[min(24rem,calc(100vw-24px))] p-3"
      >
        {open && (
          <ReviewChangesFormContainer
            workspaceId={workspaceId}
            reviewer={reviewer}
            turn={turn}
            onDone={() => setOpen(false)}
          />
        )}
      </PopoverContent>
    </Popover>
  );
}

function ReviewChangesFormContainer({
  workspaceId,
  reviewer,
  turn,
  onDone,
}: {
  workspaceId: string;
  reviewer: DiffReviewerContext;
  turn?: { id: string; label: string };
  onDone: () => void;
}) {
  const choices = useMemo(
    () => reviewEngineChoices(reviewer.harnesses),
    [reviewer.harnesses],
  );
  const initial = useMemo(
    () => defaultReviewEngine(choices, reviewer.author, rememberedEngine()),
    // The form opens on one default; later doctor refreshes do not move it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );
  const [engine, setEngine] = useState<HarnessKind | null>(
    initial?.kind ?? null,
  );
  const [modelsByEngine, setModelsByEngine] = useState<
    Partial<Record<HarnessKind, string>>
  >({});
  const [modelOptions, setModelOptions] = useState<CodeModelOption[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const scopes = useMemo<ReviewScopeChoice[]>(
    () =>
      turn
        ? [
            { value: turn.id, label: turn.label },
            { value: WORKING_TREE, label: "Working tree" },
          ]
        : [{ value: WORKING_TREE, label: "Working tree" }],
    [turn],
  );
  const [scope, setScope] = useState(scopes[0]!.value);
  const [error, setError] = useState<string | null>(null);
  const starting = useCodeReviewStore((state) =>
    Boolean(state.starting[workspaceId]),
  );
  const ensureHarnessModels = useCodeCatalogStore(
    (state) => state.ensureHarnessModels,
  );

  useEffect(() => {
    if (!engine) return;
    const gateway = gatewayCodeModels(
      reviewer.catalogModels ?? [],
      engine,
      reviewer.defaultModelKey,
    );
    const apply = (listed: readonly CodeModelOption[]) => {
      const options = preferredCodeModels(engine, listed, gateway);
      setModelOptions(options);
      setModelsByEngine((current) => {
        const kept = current[engine];
        if (kept && options.some((option) => option.id === kept)) {
          return current;
        }
        const picked =
          options.find((option) => option.default)?.id ?? options[0]?.id;
        return picked ? { ...current, [engine]: picked } : current;
      });
      setModelLoading(false);
    };
    const client = reviewer.client;
    if (!client.listCodeHarnessModels) {
      apply([]);
      return;
    }
    let cancelled = false;
    setModelLoading(true);
    void ensureHarnessModels(
      client as Pick<ApiClient, "listCodeHarnessModels">,
      engine,
    ).then((listed) => {
      if (!cancelled) apply(listed);
    });
    return () => {
      cancelled = true;
    };
  }, [
    engine,
    ensureHarnessModels,
    reviewer.catalogModels,
    reviewer.client,
    reviewer.defaultModelKey,
  ]);

  async function start() {
    if (!engine) return;
    setError(null);
    rememberEngine(engine);
    const model = modelsByEngine[engine];
    try {
      await useCodeReviewStore.getState().start(reviewer.client, workspaceId, {
        session_id: reviewer.sessionId,
        harness: engine,
        ...(model ? { model } : {}),
        ...(scope !== WORKING_TREE ? { turn_id: scope } : {}),
        instructions: renderWorkflowPrompt("review_changes", {}),
      });
      onDone();
    } catch (err) {
      setError(friendlyErrorMessage(err, "Could not start the review"));
    }
  }

  return (
    <ReviewChangesForm
      choices={choices}
      author={reviewer.author}
      engine={engine}
      onEngineChange={(next) => {
        setError(null);
        setModelOptions([]);
        setEngine(next);
      }}
      modelOptions={modelOptions}
      model={engine ? modelsByEngine[engine] : undefined}
      modelLoading={modelLoading}
      onModelChange={(next) => {
        if (!engine) return;
        setModelsByEngine((current) => ({ ...current, [engine]: next }));
      }}
      scopes={scopes}
      scope={scope}
      onScopeChange={setScope}
      starting={starting}
      error={error}
      onStart={() => void start()}
      onCancel={onDone}
    />
  );
}

/** "the working tree" or "turn 3", as a sentence names what was reviewed. */
function scopeWords(review: CodeReviewSnapshot, turnLabel?: string): string {
  if (!review.turn_id) return "the working tree";
  return turnLabel ? turnLabel.toLowerCase() : "a turn";
}

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`;
}

/** What the reviewer is doing, in one muted line. */
export function reviewProgressLine(review: CodeReviewSnapshot): string {
  const { progress } = review;
  const parts = [
    progress.activity ?? null,
    progress.files_read > 0
      ? plural(progress.files_read, "file read", "files read")
      : null,
    progress.refused > 0
      ? plural(progress.refused, "change refused", "changes refused")
      : null,
  ].filter((part): part is string => Boolean(part));
  return parts.join(" · ");
}

/** How many findings a completed review returned, on the diff or off it. */
export function reviewFindingCount(review: CodeReviewSnapshot): number {
  const result = review.result;
  return result ? result.findings.length + result.unplaced.length : 0;
}

/** The status line's first sentence. */
export function reviewHeadline(
  review: CodeReviewSnapshot,
  turnLabel?: string,
): string {
  const label = HARNESS_LABELS[review.harness];
  switch (review.status) {
    case "running":
      return `${label} is reviewing ${scopeWords(review, turnLabel)}`;
    case "completed": {
      if (review.result?.raw_text !== undefined) {
        return `${label} answered, but not as findings`;
      }
      const count = reviewFindingCount(review);
      return count === 0
        ? `${label} found nothing to change`
        : `${label} reported ${plural(count, "finding", "findings")}`;
    }
    case "failed":
    case "timed_out":
      // The detail says why, in its own words; the headline says what came
      // of it, so the two never say the same thing twice.
      return `${label} couldn't finish the review`;
    case "cancelled":
      return "Review stopped";
  }
}

/**
 * How a review's status reads: the notice's edge, and the mark beside the
 * headline. A clean review finished well; a limit or a time-out needs a
 * look; anything else that stopped it failed.
 */
export function reviewTone(review: CodeReviewSnapshot): StatusTone {
  switch (review.status) {
    case "running":
      return "running";
    case "completed":
      return reviewFindingCount(review) === 0 &&
        review.result?.raw_text === undefined
        ? "ready"
        : "neutral";
    case "failed":
      return review.failure?.kind === "rate_limited" ? "warning" : "critical";
    case "timed_out":
      return "warning";
    case "cancelled":
      return "neutral";
  }
}

const NOTICE_TONE: Partial<Record<StatusTone, string>> = {
  ready: "notice-success",
  warning: "notice-warning",
  critical: "notice-critical",
};

function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [active]);
  return now;
}

/**
 * How a review stands, under the diff's header: while it runs, what the
 * reviewer is doing, for how long, and a way to stop it; once it ends, what
 * it found, or why it stopped, and what to do next.
 */
export function ReviewStatus({
  review,
  now,
  turnLabel,
  proposed = 0,
  stopping = false,
  onStop,
  onKeepAll,
  onDismissAll,
  onRetry,
  onOpenEngines,
  onClose,
}: {
  review: CodeReviewSnapshot;
  /** The clock, in epoch milliseconds, for the elapsed time. */
  now: number;
  turnLabel?: string;
  /** Findings from this review still waiting to be kept or dismissed. */
  proposed?: number;
  stopping?: boolean;
  onStop?: () => void;
  onKeepAll?: () => void;
  onDismissAll?: () => void;
  onRetry?: () => void;
  onOpenEngines?: () => void;
  onClose?: () => void;
}) {
  const running = review.status === "running";
  const headline =
    stopping && running
      ? `Stopping ${HARNESS_LABELS[review.harness]}'s review`
      : reviewHeadline(review, turnLabel);
  const tone = reviewTone(review);
  const elapsed = formatElapsedDuration(
    now - new Date(review.started_at).getTime(),
  );
  const detail =
    review.status === "completed"
      ? (review.result?.summary ?? null)
      : review.status === "cancelled"
        ? "Nothing was added to the diff."
        : (review.failure?.message ?? null);
  const rejected = review.result?.rejected ?? 0;
  const omitted = review.result?.omitted_diffs ?? 0;
  const credential =
    review.failure?.kind === "signed_out" ||
    review.failure?.kind === "not_installed";
  const Mark =
    tone === "ready"
      ? CircleCheck
      : tone === "warning" || tone === "critical"
        ? CircleAlert
        : ScanSearch;
  return (
    <section
      aria-label="Review"
      className={cn(
        "notice-surface flex shrink-0 flex-col gap-1 border-b px-3 py-2",
        NOTICE_TONE[tone],
      )}
      data-review-status={review.status}
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        {running ? (
          <Loader
            variant="comet"
            size={14}
            className="text-live shrink-0"
            decorative
          />
        ) : (
          <Mark
            className={cn("size-3.5 shrink-0", STATUS_MARK[tone])}
            aria-hidden
          />
        )}
        <p className="min-w-0 text-sm font-medium" role="status">
          {running ? <LiveLabel live>{headline}</LiveLabel> : headline}
        </p>
        <span className="text-muted-foreground min-w-0 truncate text-xs">
          {[review.model ?? null, running ? elapsed : null]
            .filter(Boolean)
            .join(" · ")}
        </span>
        <span className="ml-auto flex shrink-0 items-center gap-1">
          {running && onStop && (
            <Button
              type="button"
              variant="ghost"
              size="xs"
              disabled={stopping}
              onClick={onStop}
            >
              {stopping ? <Spinner aria-hidden /> : null}
              {stopping ? "Stopping…" : "Stop review"}
            </Button>
          )}
          {proposed > 0 && onKeepAll && (
            <Button type="button" variant="ghost" size="xs" onClick={onKeepAll}>
              Keep all
            </Button>
          )}
          {proposed > 0 && onDismissAll && (
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={onDismissAll}
            >
              Dismiss all
            </Button>
          )}
          {credential && onOpenEngines && (
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={onOpenEngines}
            >
              Coding engines
            </Button>
          )}
          {(review.status === "failed" || review.status === "timed_out") &&
            onRetry && (
              <Button type="button" variant="ghost" size="xs" onClick={onRetry}>
                Try again
              </Button>
            )}
          {!running && onClose && (
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label="Close the review status"
              onClick={onClose}
            >
              <X aria-hidden />
            </Button>
          )}
        </span>
      </div>
      {running && reviewProgressLine(review) && (
        <p className="text-muted-foreground truncate pl-5.5 text-xs">
          {reviewProgressLine(review)}
        </p>
      )}
      {detail && (
        <p className="text-muted-foreground line-clamp-3 pl-5.5 text-xs break-words">
          {detail}
        </p>
      )}
      {review.status === "completed" && proposed > 0 && (
        <p className="text-muted-foreground pl-5.5 text-xs">
          {plural(proposed, "finding waits", "findings wait")} in the diff for
          you to keep or dismiss. Kept ones go with your next message.
        </p>
      )}
      {review.status === "completed" && rejected > 0 && (
        <p className="text-muted-foreground pl-5.5 text-xs">
          {plural(rejected, "entry", "entries")} in the answer could not be read
          and {rejected === 1 ? "was" : "were"} left out.
        </p>
      )}
      {review.status === "completed" && omitted > 0 && (
        <p className="text-muted-foreground pl-5.5 text-xs">
          The review found more than one message can carry, so the findings on{" "}
          {plural(omitted, "file", "files")} are listed above the files instead
          of on their lines.
        </p>
      )}
    </section>
  );
}

/** The status of the workspace's latest review, wired to the stores. */
export function DiffReviewStatus({
  workspaceId,
  reviewer,
  turnLabel,
}: {
  workspaceId: string;
  reviewer: DiffReviewerContext;
  turnLabel?: string;
}) {
  const review = useWorkspaceReview(workspaceId);
  const { comments } = usePendingReview(workspaceId);
  // The status also renders in stories and tests with no router around it.
  const router = useRouter({ warn: false }) as
    | ReturnType<typeof useRouter>
    | undefined;
  const harnessesPath: string = "/settings/coding-harnesses";
  const stopping = useReviewStopping(workspaceId, review?.id);
  const now = useNow(review?.status === "running");
  const client = reviewer.client;
  useEffect(() => {
    void useCodeReviewStore
      .getState()
      .refresh(client, workspaceId)
      .catch(() => undefined);
  }, [client, workspaceId]);
  if (!review) return null;
  const proposed = comments.filter(
    (comment) =>
      comment.proposed &&
      comment.author.kind === "reviewer" &&
      comment.author.reviewId === review.id,
  ).length;
  return (
    <ReviewStatus
      review={review}
      now={now}
      turnLabel={turnLabel}
      proposed={proposed}
      stopping={stopping}
      onStop={() => {
        // Stopping lasts until the server says the review ended: the engine
        // gets a few seconds to wind down first.
        void useCodeReviewStore
          .getState()
          .cancel(client, workspaceId)
          .catch((err: unknown) =>
            toast.error(friendlyErrorMessage(err, "Could not stop the review")),
          );
      }}
      onKeepAll={() =>
        usePendingReviewStore.getState().keepAll(workspaceId, review.id)
      }
      onDismissAll={() =>
        usePendingReviewStore.getState().dismissAll(workspaceId, review.id)
      }
      onRetry={() => useCodeUiStore.getState().requestReviewForm(workspaceId)}
      onOpenEngines={() => void router?.navigate({ to: harnessesPath })}
      onClose={() => useCodeReviewStore.getState().close(workspaceId)}
    />
  );
}
