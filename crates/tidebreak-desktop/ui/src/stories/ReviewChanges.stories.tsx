import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, waitFor, within } from "storybook/test";

import type {
  CodeReviewFinding,
  CodeReviewSnapshot,
  HarnessDoctorEntry,
  HarnessKind,
} from "@/api/types";
import { DiffPanel } from "@/code/DiffPanel";
import { useDiffPreferences } from "@/code/diff/diffPreferences";
import { usePendingReviewStore } from "@/code/diff/pendingReview";
import { commentsFromReview } from "@/code/diff/reviewFindings";
import type { DiffReviewerContext } from "@/code/review/ReviewChanges";
import { useCodeReviewStore } from "@/code/review/reviewStore";
import { groupUnifiedDiff } from "@/code/unifiedDiff";
import { QUEUE_DIFF, QUEUE_PATH } from "./diffFixtures";
import { harnessDoctor } from "./fixtures";

/**
 * Review changes: another engine reads the workspace's changes, read-only,
 * and its findings land in the diff as comments to keep or dismiss. These
 * stories show the diff as the person sees it through a review: choosing
 * the engine, the review running, how it can end, and what it found.
 */

type Story = StoryObj<typeof meta>;

type ReviewStoryProps = {
  /** The workspace the story's stores are seeded for. */
  workspaceId: string;
  /** The review as the server reports it, or none yet. */
  review: CodeReviewSnapshot | null;
  /** Engines the machine has, as the doctor reports them. */
  harnesses?: readonly HarnessDoctorEntry[];
  /** The turn the diff shows, when it shows one. */
  turn?: { id: string; label: string };
};

const MODELS: Record<
  HarnessKind,
  readonly { id: string; label: string; default?: boolean }[]
> = {
  codex: [
    { id: "gpt-5.5", label: "GPT-5.5", default: true },
    { id: "gpt-5.5-mini", label: "GPT-5.5 mini" },
  ],
  claude_code: [
    { id: "claude-opus-5", label: "Claude Opus 5", default: true },
    { id: "claude-sonnet-5", label: "Claude Sonnet 5" },
  ],
  opencode: [{ id: "model-gateway/glm-5.3", label: "GLM 5.3", default: true }],
  grok: [{ id: "grok-4.6", label: "Grok 4.6", default: true }],
  internal: [],
};

function stat(diff: string) {
  const lines = diff.split("\n");
  return {
    files: groupUnifiedDiff(diff).length,
    insertions: lines.filter((line) => /^\+(?!\+\+ )/.test(line)).length,
    deletions: lines.filter((line) => /^-(?!-- )/.test(line)).length,
    truncated: false,
  };
}

/** A local server for one workspace, answering with the story's review. */
function reviewer(
  workspaceId: string,
  review: CodeReviewSnapshot | null,
  harnesses: readonly HarnessDoctorEntry[],
): DiffReviewerContext {
  const latest = () => (review ? [review] : []);
  return {
    client: {
      startCodeReview: async () =>
        review ??
        running(workspaceId, { started_at: new Date().toISOString() }),
      getCodeReview: async () => review ?? running(workspaceId),
      cancelCodeReview: async () => ({
        ...(review ?? running(workspaceId)),
        status: "cancelled" as const,
      }),
      listCodeReviews: async () => latest(),
      listCodeHarnessModels: async (kind) => ({
        kind,
        models: [...MODELS[kind]].map((model) => ({
          ...model,
          default: model.default ?? false,
          reasoning_efforts: [],
          fast_mode: false,
        })),
        reasoning_efforts: [],
        source: "harness" as const,
      }),
    },
    sessionId: "sess-review",
    author: "claude_code",
    harnesses,
  };
}

function ReviewStory({
  workspaceId,
  review,
  harnesses = harnessDoctor.harnesses,
  turn,
}: ReviewStoryProps) {
  return (
    <div
      className="bg-background flex min-h-0 flex-col overflow-hidden rounded-lg border"
      style={{ height: 620 }}
    >
      <DiffPanel
        client={{
          getCodeWorkspaceDiff: async () => ({
            diff: QUEUE_DIFF,
            truncated: false,
            stat: stat(QUEUE_DIFF),
          }),
        }}
        workspaceId={workspaceId}
        turnId={turn?.id}
        turnLabel={turn?.label}
        onOpenFile={() => {}}
        reviewer={reviewer(workspaceId, review, harnesses)}
      />
    </div>
  );
}

function running(
  workspaceId: string,
  overrides: Partial<CodeReviewSnapshot> = {},
): CodeReviewSnapshot {
  return {
    id: `rev-${workspaceId}`,
    workspace_id: workspaceId,
    session_id: "sess-review",
    harness: "codex",
    model: "gpt-5.5",
    permission_mode: "plan",
    status: "running",
    progress: {
      tool_calls: 6,
      files_read: 4,
      refused: 0,
      activity: `Reading ${QUEUE_PATH.split("/").pop()}`,
    },
    // Forty-two seconds in, whenever the story renders.
    started_at: new Date(Date.now() - 42_000).toISOString(),
    ...overrides,
  };
}

const FINDINGS: CodeReviewFinding[] = [
  {
    path: QUEUE_PATH,
    start_line: 29,
    end_line: 30,
    severity: "high",
    title: "Queued messages lose their attachments",
    explanation:
      "enqueue builds the message without the caller's attachments and then sets them to an empty list, so an image attached to a queued message is dropped. Take the attachments as a parameter and keep them.",
  },
  {
    path: QUEUE_PATH,
    start_line: 23,
    end_line: 23,
    severity: "medium",
    title: "The queue limit doubled without a reason",
    explanation:
      "MAX_QUEUED went from 10 to 20, but nothing in the change says why, and the tray still sizes itself for ten rows. Keep 10, or grow the tray and say why.",
  },
];

const OFF_DIFF: CodeReviewFinding[] = [
  {
    path: QUEUE_PATH,
    start_line: 90,
    end_line: 92,
    severity: "low",
    title: "dequeue signals the tray before it returns",
    explanation:
      "The tray can read the queue before the caller stores what dequeue returned. Signal after the caller has the new queue.",
  },
];

function completed(
  workspaceId: string,
  result: Partial<NonNullable<CodeReviewSnapshot["result"]>>,
): CodeReviewSnapshot {
  return running(workspaceId, {
    status: "completed",
    finished_at: new Date().toISOString(),
    progress: { tool_calls: 11, files_read: 6, refused: 1 },
    result: {
      findings: [],
      unplaced: [],
      rejected: 0,
      diff: QUEUE_DIFF,
      ...result,
    },
  });
}

/**
 * Seed the stores the way a finished review leaves them: the review, and
 * its findings in the workspace's pending review.
 */
function seed(workspaceId: string, review: CodeReviewSnapshot | null) {
  return async () => {
    useDiffPreferences.getState().setLayout("unified");
    useCodeReviewStore.setState((state) => {
      const byWorkspace = { ...state.byWorkspace };
      if (review) byWorkspace[workspaceId] = review;
      else delete byWorkspace[workspaceId];
      const closed = { ...state.closed };
      delete closed[workspaceId];
      return { byWorkspace, closed };
    });
    const findings = review ? commentsFromReview(review) : [];
    usePendingReviewStore.setState((state) => ({
      byWorkspace: { ...state.byWorkspace, [workspaceId]: findings },
      imported: {
        ...state.imported,
        [workspaceId]: review ? [review.id] : [],
      },
    }));
    return {};
  };
}

const meta = {
  title: "Code/Review changes",
  component: ReviewStory,
  parameters: { layout: "padded" },
  args: { workspaceId: "ws-review-form", review: null },
  loaders: [seed("ws-review-form", null)],
} satisfies Meta<typeof ReviewStory>;

export default meta;

/**
 * The form opens in a popover on the document body, not in the story root,
 * and fades in: wait until what `find` returns has finished opening.
 */
async function waitUntilShown(find: () => HTMLElement) {
  await waitFor(() => expect(find()).toBeVisible());
}

/**
 * The form the diff's header opens: the engine starts on one that did not
 * write the changes, the model it will run on is in view, and nothing runs
 * until Start review.
 */
export const ChoosingAnEngine: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Review changes" }),
    );
    const body = within(canvasElement.ownerDocument.body);
    await waitUntilShown(() =>
      body.getByRole("button", { name: "Start review" }),
    );
    await waitFor(() =>
      expect(body.getByRole("combobox")).toHaveTextContent("Codex CLI"),
    );
    await waitUntilShown(() =>
      body.getByRole("button", { name: "Model: GPT 5.5" }),
    );
  },
};

/**
 * On one turn's diff, the form offers that turn first, and the working
 * tree beside it.
 */
export const ChoosingATurn: Story = {
  args: {
    workspaceId: "ws-review-turn",
    turn: { id: "turn-3", label: "Turn 3" },
  },
  loaders: [seed("ws-review-turn", null)],
  play: ChoosingAnEngine.play,
};

/**
 * Only the engine that wrote the changes is ready: it reviews its own
 * changes, and the form says so.
 */
export const OnlyTheAuthorIsReady: Story = {
  args: {
    workspaceId: "ws-review-alone",
    harnesses: harnessDoctor.harnesses.map((entry) =>
      entry.kind === "claude_code" ? entry : { ...entry, found: false },
    ),
  },
  loaders: [seed("ws-review-alone", null)],
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByRole("button", {
        name: "Review changes",
      }),
    );
    await waitUntilShown(() =>
      within(canvasElement.ownerDocument.body).getByText(
        "No other engine is ready, so Claude Code reviews its own changes.",
      ),
    );
  },
};

/** Running: what the reviewer is reading, for how long, and Stop review. */
export const Running: Story = {
  args: {
    workspaceId: "ws-review-running",
    review: running("ws-review-running"),
  },
  loaders: [seed("ws-review-running", running("ws-review-running"))],
  play: async ({ canvasElement }) => {
    await expect(
      within(canvasElement).findByRole("button", { name: "Stop review" }),
    ).resolves.toBeVisible();
  },
};

const WITH_FINDINGS = completed("ws-review-findings", {
  summary:
    "The queue change drops attachments on queued messages; the rest reads well.",
  findings: FINDINGS,
  unplaced: OFF_DIFF,
});

/**
 * Findings in the diff: each under the lines it names, with its engine,
 * severity, and title, waiting to be kept or dismissed. One on lines the diff
 * does not show sits above the files, about the changes as a whole.
 */
export const FindingsInTheDiff: Story = {
  args: { workspaceId: "ws-review-findings", review: WITH_FINDINGS },
  loaders: [seed("ws-review-findings", WITH_FINDINGS)],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("Queued messages lose their attachments"),
    ).resolves.toBeVisible();
    await expect(
      canvas.findByRole("button", { name: "Keep all" }),
    ).resolves.toBeVisible();
  },
};

const NOTHING_FOUND = completed("ws-review-clean", {
  summary: "The change does what it says, and the tests cover the new limit.",
});

/** The reviewer found nothing to change, and says what it looked at. */
export const NoFindings: Story = {
  args: { workspaceId: "ws-review-clean", review: NOTHING_FOUND },
  loaders: [seed("ws-review-clean", NOTHING_FOUND)],
};

const SIGNED_OUT = running("ws-review-failed", {
  status: "failed",
  finished_at: new Date().toISOString(),
  progress: { tool_calls: 0, files_read: 0, refused: 0 },
  failure: {
    kind: "signed_out",
    message:
      "Codex CLI is not signed in, or its sign-in was refused. Sign in from Settings > Coding engines. The engine said: 401 Unauthorized",
  },
});

/** Failed: why, in the engine's own words, and the way back. */
export const Failed: Story = {
  args: { workspaceId: "ws-review-failed", review: SIGNED_OUT },
  loaders: [seed("ws-review-failed", SIGNED_OUT)],
};

const RATE_LIMITED = running("ws-review-limited", {
  status: "failed",
  harness: "claude_code",
  model: "claude-opus-5",
  finished_at: new Date().toISOString(),
  failure: {
    kind: "rate_limited",
    message:
      "Claude Code hit a rate or usage limit. Try again later, or pick another engine. The engine said: Claude AI usage limit reached",
  },
});

/** A provider limit reads as its own failure, with another engine offered. */
export const RateLimited: Story = {
  args: { workspaceId: "ws-review-limited", review: RATE_LIMITED },
  loaders: [seed("ws-review-limited", RATE_LIMITED)],
};

const TIMED_OUT = running("ws-review-timeout", {
  status: "timed_out",
  finished_at: new Date().toISOString(),
  failure: {
    kind: "timed_out",
    message:
      "Codex CLI ran past 20 minutes and was stopped. Try again, or review one turn's changes instead.",
  },
});

/** The review ran past its time limit and was stopped. */
export const TimedOut: Story = {
  args: { workspaceId: "ws-review-timeout", review: TIMED_OUT },
  loaders: [seed("ws-review-timeout", TIMED_OUT)],
};

const CANCELLED = running("ws-review-cancelled", {
  status: "cancelled",
  finished_at: new Date().toISOString(),
});

/** Stopped by the person: nothing was added. */
export const Cancelled: Story = {
  args: { workspaceId: "ws-review-cancelled", review: CANCELLED },
  loaders: [seed("ws-review-cancelled", CANCELLED)],
};
