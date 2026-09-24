import { useEffect, useMemo, useRef, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, waitFor, within } from "storybook/test";

import type { ApiClient } from "@/api/client";
import type { CodeWorkspaceDiff, QueuedCodeTurn } from "@/api/types";
import { codeQueueApi, QueueTray } from "@/QueueTray";
import { DiffPanel, type DiffRevertActions } from "@/code/DiffPanel";
import {
  useDiffPreferences,
  type DiffLayout,
} from "@/code/diff/diffPreferences";
import { DIFF_CHUNK_ROWS, DiffView } from "@/code/diff/DiffView";
import { usePendingReviewStore } from "@/code/diff/pendingReview";
import {
  messageWithReviewComments,
  reviewBlockOf,
  type ReviewComment,
  type ReviewCommentLine,
} from "@/code/diff/reviewComments";
import { diffRows } from "@/code/diff/diffModel";
import { groupUnifiedDiff } from "@/code/unifiedDiff";
import {
  BINARY_DIFF,
  CHECKPOINT_DIFF,
  CHECKPOINT_PATH,
  GENERATED_PATH,
  LAYOUT_PATH,
  LOGO_PATH,
  QUEUE_DIFF,
  QUEUE_PATH,
  REINDENT_DIFF,
  WHITESPACE_ONLY_DIFF,
  longFileDiff,
} from "./diffFixtures";

type DiffStoryProps = {
  /** The unified diff the fake server answers with. */
  diff: string;
  path?: string;
  /** The workspace whose pending review the story seeds. */
  workspaceId: string;
  height?: number;
  /** Offer Revert on the file and its hunks. */
  revertable?: boolean;
};

/** Reverts that ask nothing and land at once, for the stories. */
const STORY_REVERTS: DiffRevertActions = {
  onRevertFile: async () => true,
  onRevertHunk: async () => true,
};

function stat(diff: string): CodeWorkspaceDiff["stat"] {
  const lines = diff.split("\n");
  const groups = groupUnifiedDiff(diff);
  return {
    files: groups.length,
    insertions: lines.filter((line) => /^\+(?!\+\+ )/.test(line)).length,
    deletions: lines.filter((line) => /^-(?!-- )/.test(line)).length,
    truncated: false,
  };
}

function clientFor(
  diff: string,
): Pick<ApiClient, "getCodeWorkspaceDiff" | "listCodeWorkspaceFiles"> {
  return {
    getCodeWorkspaceDiff: async (_workspace, options) => ({
      diff,
      truncated: false,
      stat: stat(diff),
      ...(options?.file ? { file: options.file } : {}),
    }),
    listCodeWorkspaceFiles: async () => ({
      files: groupUnifiedDiff(diff).map((group) => ({
        path: group.path,
        kind: "modified" as const,
        insertions: 0,
        deletions: 0,
      })),
      truncated: false,
      stat: stat(diff),
    }),
  };
}

/** The workspace diff surface as the center tab shows it. */
function DiffStory({
  diff,
  path,
  workspaceId,
  height = 560,
  revertable = false,
}: DiffStoryProps) {
  return (
    <div
      className="bg-background flex min-h-0 flex-col overflow-hidden rounded-lg border"
      style={{ height }}
    >
      <DiffPanel
        client={clientFor(diff)}
        workspaceId={workspaceId}
        file={path}
        onOpenFile={() => {}}
        revert={revertable ? STORY_REVERTS : undefined}
      />
    </div>
  );
}

function line(
  kind: ReviewCommentLine["kind"],
  oldNo: number | null,
  newNo: number | null,
  text: string,
): ReviewCommentLine {
  return { kind, oldNo, newNo, text };
}

function comment(
  id: string,
  path: string,
  lines: ReviewCommentLine[],
  body: string,
): ReviewComment {
  return {
    id,
    author: { kind: "person" },
    path,
    lines,
    body,
    createdAt: "2026-09-24T10:00:00.000Z",
  };
}

const QUEUE_COMMENTS: ReviewComment[] = [
  comment(
    "c-queue-type",
    QUEUE_PATH,
    [
      line("add", null, 18, "  message: string;"),
      line("add", null, 19, "  attachments: readonly CodeTurnImage[];"),
    ],
    "Rename the field back to `text`. The server journals it under that name, and a rename here means a migration.",
  ),
  comment(
    "c-queue-limit",
    QUEUE_PATH,
    [line("del", 22, null, "const MAX_QUEUED = 10;")],
    "Why double it? The tray was designed around ten rows.",
  ),
  comment(
    "c-queue-gone",
    QUEUE_PATH,
    [line("add", null, 140, "  return drained;")],
    "This is the line that returned the drained queue before the last turn moved it.",
  ),
];

/**
 * Set the story's remembered layout, and seed its workspace with
 * `comments`, some of them riding a send.
 */
function seedReview(
  workspaceId: string,
  comments: readonly ReviewComment[],
  sending: readonly string[] = [],
  layout: DiffLayout = "unified",
) {
  return async () => {
    useDiffPreferences.getState().setLayout(layout);
    usePendingReviewStore.setState((state) => ({
      byWorkspace: { ...state.byWorkspace, [workspaceId]: [...comments] },
      sending: { ...state.sending, [workspaceId]: [...sending] },
    }));
    return {};
  };
}

/**
 * The review surface for a diff: syntax color under the add and delete
 * tints, word emphasis inside changed lines, unified or side by side, and
 * line comments that wait for the next message.
 */
const meta = {
  title: "Code/Diff review",
  component: DiffStory,
  args: {
    diff: QUEUE_DIFF,
    path: QUEUE_PATH,
    workspaceId: "ws-diff-story",
  },
  loaders: [seedReview("ws-diff-story", [])],
} satisfies Meta<typeof DiffStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** One file, unified: both gutters, one marker column, one type scale. */
export const Unified: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByRole("button", { name: "Comment on line 12" }),
    ).resolves.toBeVisible();
  },
};

/** The same file side by side, removed lines paired with their additions. */
export const Split: Story = {
  args: { workspaceId: "ws-diff-split" },
  loaders: [seedReview("ws-diff-split", [], [], "split")],
  play: async ({ canvasElement }) => {
    await waitFor(() =>
      expect(
        canvasElement.querySelector('[data-diff-view="split"]'),
      ).not.toBeNull(),
    );
  },
};

/**
 * Side by side with Revert on each hunk. The hunk's action cell grows past
 * the single gutter each side has, so the word reads whole.
 */
export const SplitWithRevert: Story = {
  args: { workspaceId: "ws-diff-split-revert", revertable: true },
  loaders: [seedReview("ws-diff-split-revert", [], [], "split")],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() =>
      expect(
        canvasElement.querySelector('[data-diff-view="split"]'),
      ).not.toBeNull(),
    );
    const [revert] = await canvas.findAllByRole("button", {
      name: /^Revert the change at/,
    });
    const cell = revert!.closest<HTMLElement>('[data-diff-gutter="action"]')!;
    // The button fits inside its cell rather than spilling out of its left edge.
    await expect(revert!.getBoundingClientRect().left).toBeGreaterThanOrEqual(
      cell.getBoundingClientRect().left,
    );
  },
};

/**
 * Hunks that open inside a block comment and carry a raw string across
 * lines. Each side of a hunk is highlighted as one run, so both read the
 * way they do in the file.
 */
export const Highlighted: Story = {
  args: {
    diff: CHECKPOINT_DIFF,
    path: CHECKPOINT_PATH,
    workspaceId: "ws-diff-highlight",
  },
  loaders: [seedReview("ws-diff-highlight", [])],
  play: async ({ canvasElement }) => {
    await waitFor(() =>
      expect(
        canvasElement.querySelector(".text-syntax-keyword"),
      ).not.toBeNull(),
    );
  },
};

/** Paired lines mark only the words that changed. */
export const WordLevel: Story = {
  args: { workspaceId: "ws-diff-words" },
  loaders: [seedReview("ws-diff-words", [])],
  play: async ({ canvasElement }) => {
    await waitFor(() =>
      expect(canvasElement.querySelector("mark")).not.toBeNull(),
    );
  },
};

/**
 * A block moved under a condition. Hiding whitespace leaves the two lines
 * that really changed; the re-indented ones read as context.
 */
export const WhitespaceIgnored: Story = {
  args: {
    diff: REINDENT_DIFF,
    path: LAYOUT_PATH,
    workspaceId: "ws-diff-whitespace",
  },
  loaders: [seedReview("ws-diff-whitespace", [])],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Hide whitespace changes" }),
    );
    await expect(
      canvas.findByRole("button", { name: "Hide whitespace changes" }),
    ).resolves.toHaveAttribute("aria-pressed", "true");
  },
};

/** Every change was whitespace: hiding it says so and offers the way back. */
export const WhitespaceOnly: Story = {
  args: {
    diff: WHITESPACE_ONLY_DIFF,
    path: LAYOUT_PATH,
    workspaceId: "ws-diff-whitespace-only",
  },
  loaders: [seedReview("ws-diff-whitespace-only", [])],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Hide whitespace changes" }),
    );
    await expect(
      canvas.findByText("Only whitespace changed in this file."),
    ).resolves.toBeVisible();
  },
};

/**
 * Comments waiting for the next message, under the lines they are about.
 * One quotes a line the agent has since changed, so it sits at the top as
 * outdated, with its quote.
 */
export const PendingComments: Story = {
  args: { workspaceId: "ws-diff-pending" },
  loaders: [seedReview("ws-diff-pending", QUEUE_COMMENTS)],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText(/Rename the field back/),
    ).resolves.toBeVisible();
  },
};

/**
 * A comment whose code changed after it was written. It never moves onto
 * whatever now sits at its old line number: it keeps its quote, says it is
 * outdated, and the message that carries it says so too.
 */
export const OutdatedComment: Story = {
  args: { workspaceId: "ws-diff-outdated" },
  loaders: [
    seedReview("ws-diff-outdated", [
      comment(
        "c-outdated",
        QUEUE_PATH,
        [line("add", null, 20, "  createdAt: Date;")],
        "Store this as the server's ISO string, not a Date: the journal compares them as text.",
      ),
    ]),
  ],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.findByText("Outdated")).resolves.toBeVisible();
  },
};

/**
 * With whitespace hidden, a hunk that only changed indentation leaves the
 * view. The comment on it is not outdated: it waits at the top with its
 * quote until whitespace is shown again.
 */
export const HiddenWithWhitespace: Story = {
  args: {
    diff: REINDENT_DIFF,
    path: LAYOUT_PATH,
    workspaceId: "ws-diff-hidden",
  },
  loaders: [
    seedReview("ws-diff-hidden", [
      comment(
        "c-hidden",
        LAYOUT_PATH,
        [line("add", null, 64, "  layoutPanels();")],
        "Keep tabs in this file; the formatter expects them.",
      ),
    ]),
  ],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Hide whitespace changes" }),
    );
    await expect(
      canvas.findByText(
        "Hiding whitespace leaves out the lines these comments are on.",
      ),
    ).resolves.toBeVisible();
  },
};

/**
 * The agent reverted a file the reader had commented on. The file stays
 * first in the list, under its path, with its comments outdated and quoted,
 * and they go to the agent marked outdated.
 */
export const FileLeftTheDiff: Story = {
  args: {
    diff: CHECKPOINT_DIFF,
    path: undefined,
    workspaceId: "ws-diff-gone",
  },
  loaders: [seedReview("ws-diff-gone", QUEUE_COMMENTS.slice(0, 2))],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("No longer in this diff"),
    ).resolves.toBeVisible();
    await expect(canvas.findAllByText("Outdated")).resolves.toHaveLength(2);
  },
};

/**
 * Written with whitespace hidden, on a line the agent only re-indented. The
 * queued message's text names the line, but not its old indentation.
 */
const RESTORED_COMMENT: ReviewComment = {
  ...comment(
    "c-restored",
    LAYOUT_PATH,
    [
      {
        kind: "context",
        oldNo: 21,
        newNo: 22,
        text: "    useEffect(() => {",
        oldText: "  useEffect(() => {",
      },
    ],
    "Hooks cannot sit under a condition. Keep the effect at the top and check `inspector` inside it.",
  ),
  context: {
    before: ["  if (inspector) {", "  const [open, setOpen] = useState(true);"],
    after: [
      '      window.addEventListener("resize", onResize);',
      '      return () => window.removeEventListener("resize", onResize);',
      "    }, [onResize]);",
    ],
  },
};

const RESTORED_MESSAGE = messageWithReviewComments(
  "Then rerun the layout tests.",
  [RESTORED_COMMENT],
);

/** A code session's queue holding one message, until it is deleted. */
function queueClient(message: string): ApiClient {
  let rows: QueuedCodeTurn[] = [
    {
      id: "q-review",
      session_id: "sess-story",
      message,
      position: 0,
      created_at: "2026-09-24T10:05:00.000Z",
      updated_at: "2026-09-24T10:05:00.000Z",
    },
  ];
  return {
    listCodeQueuedTurns: async () => ({ queued: rows, paused: false }),
    deleteCodeQueuedTurn: async (_session: string, id: string) => {
      rows = rows.filter((row) => row.id !== id);
    },
  } as unknown as ApiClient;
}

function RestoredFromQueueStory({ workspaceId }: { workspaceId: string }) {
  const [client] = useState(() => queueClient(RESTORED_MESSAGE));
  const queue = useMemo(
    () => codeQueueApi(client, "sess-story", { workspaceId }),
    [client, workspaceId],
  );
  return (
    <div className="flex flex-col gap-3">
      <QueueTray queue={queue} active onStop={async () => {}} />
      <DiffStory
        diff={REINDENT_DIFF}
        path={LAYOUT_PATH}
        workspaceId={workspaceId}
        height={420}
      />
    </div>
  );
}

/**
 * Deleting a queued message gives its comments back to the review as they
 * were when it went, not as its text reads them: this one keeps its old
 * indentation and the code around it, so it returns under its line.
 */
export const RestoredFromAQueuedMessage: Story = {
  args: { workspaceId: "ws-diff-restored" },
  loaders: [
    seedReview("ws-diff-restored", []),
    async () => {
      usePendingReviewStore
        .getState()
        .keepQueued("ws-diff-restored", reviewBlockOf(RESTORED_MESSAGE)!, [
          RESTORED_COMMENT,
        ]);
      return {};
    },
  ],
  render: ({ workspaceId }) => (
    <RestoredFromQueueStory workspaceId={workspaceId} />
  ),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.findByText("1 review comment")).resolves.toBeVisible();
    await userEvent.click(
      canvas.getByRole("button", { name: "Delete queued message" }),
    );
    const card = await canvas.findByRole("article", { name: "Line 22" });
    await expect(card.closest("[data-diff-outdated]")).toBeNull();
  },
};

/** Pending comments in the side-by-side layout. */
export const PendingCommentsSplit: Story = {
  args: { workspaceId: "ws-diff-pending-split" },
  loaders: [seedReview("ws-diff-pending-split", QUEUE_COMMENTS, [], "split")],
};

/** Picking two line numbers opens the comment editor under them. */
export const WritingAComment: Story = {
  args: { workspaceId: "ws-diff-writing" },
  loaders: [seedReview("ws-diff-writing", [])],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const first = await canvas.findByRole("button", {
      name: "Comment on line 18",
    });
    await userEvent.click(first);
    const editor = await canvas.findByRole("textbox", {
      name: "Comment on line 18",
    });
    await userEvent.type(editor, "Keep the old field name.");
  },
};

/** The message went out with the comments: they hold still until it lands. */
export const Sending: Story = {
  args: { workspaceId: "ws-diff-sending" },
  loaders: [
    seedReview("ws-diff-sending", QUEUE_COMMENTS.slice(0, 2), [
      "c-queue-type",
      "c-queue-limit",
    ]),
  ],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findAllByText("Sending with your message…"),
    ).resolves.toHaveLength(2);
  },
};

/** Nothing changed in the file. */
export const Empty: Story = {
  args: { diff: "", workspaceId: "ws-diff-empty" },
  loaders: [seedReview("ws-diff-empty", [])],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("No changes in this file."),
    ).resolves.toBeVisible();
  },
};

/** Git gives no lines for a binary file. */
export const BinaryFile: Story = {
  args: { diff: BINARY_DIFF, path: LOGO_PATH, workspaceId: "ws-diff-binary" },
  loaders: [seedReview("ws-diff-binary", [])],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("Binary file not shown."),
    ).resolves.toBeVisible();
  },
};

/** The workspace against its base: several files, each under its header. */
export const WholeWorkspace: Story = {
  args: {
    diff: [QUEUE_DIFF, CHECKPOINT_DIFF, BINARY_DIFF].join("\n"),
    path: undefined,
    workspaceId: "ws-diff-workspace",
  },
  loaders: [seedReview("ws-diff-workspace", [])],
};

/**
 * A 5,000-line diff. The first chunk of rows draws on the first frame and
 * the rest follow a chunk a frame, so opening it never holds the main
 * thread. The caption reports what this browser measured.
 *
 * The measurement watches only the count of mounted chunks, one element,
 * and the story has no play function: anything that searched the DOM on
 * every frame would cost more as the rows arrived and be measured with them.
 */
export const VeryLongFile: Story = {
  args: {
    diff: longFileDiff(5_000),
    path: GENERATED_PATH,
    workspaceId: "ws-diff-long",
  },
  loaders: [seedReview("ws-diff-long", [])],
  render: (args) => <LongFileStory {...args} />,
};

type Measurement = {
  lines: number;
  firstRows: number;
  allRows: number;
  /** The task that rendered the story, Storybook's own work included. */
  mountTask: number | null;
  /** The longest task after that one, while the rest of the rows arrived. */
  longestTask: number | null;
};

function LongFileStory(props: DiffStoryProps) {
  const started = useRef(performance.now());
  const [measured, setMeasured] = useState<Measurement | null>(null);
  const firstRows = useRef<number | null>(null);
  const mountTask = useRef<number | null>(null);
  const longest = useRef<number | null>(null);
  const group = groupUnifiedDiff(props.diff)[0];
  const expected = group ? diffRows(group).length : 0;
  const expectedChunks = Math.ceil(expected / DIFF_CHUNK_ROWS);

  useEffect(() => {
    let observer: PerformanceObserver | null = null;
    try {
      observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          const end = entry.startTime + entry.duration;
          // Storybook's start-up before this story is not the diff's work.
          if (end < started.current) continue;
          if (entry.startTime <= started.current) {
            mountTask.current = Math.max(
              mountTask.current ?? 0,
              entry.duration,
            );
          } else {
            longest.current = Math.max(longest.current ?? 0, entry.duration);
          }
        }
      });
      observer.observe({ type: "longtask", buffered: true });
      mountTask.current = 0;
      longest.current = 0;
    } catch {
      // Not every engine reports long tasks; the caption says so.
    }
    let frame = 0;
    let view: Element | null = null;
    const poll = () => {
      view ??= document.querySelector("[data-diff-view]");
      // The view's last child holds one element per mounted chunk.
      const chunks = view?.lastElementChild?.childElementCount ?? 0;
      if (chunks > 0 && firstRows.current === null) {
        firstRows.current = performance.now() - started.current;
      }
      if (view && chunks >= expectedChunks) {
        const allRows = performance.now() - started.current;
        setMeasured({
          lines: view.querySelectorAll(".diff-line[data-row]").length,
          firstRows: firstRows.current ?? 0,
          allRows,
          mountTask: mountTask.current,
          longestTask: longest.current,
        });
        return;
      }
      frame = requestAnimationFrame(poll);
    };
    frame = requestAnimationFrame(poll);
    return () => {
      cancelAnimationFrame(frame);
      observer?.disconnect();
    };
  }, [expectedChunks]);

  return (
    <div className="flex flex-col gap-2">
      <p className="text-muted-foreground text-xs" role="status">
        {measured ? (
          <span data-long-diff-measured="">
            {measured.lines.toLocaleString()} lines: first rows in{" "}
            {Math.round(measured.firstRows)} ms, every row in{" "}
            {Math.round(measured.allRows)} ms.{" "}
            {measured.longestTask === null
              ? "This browser does not report long tasks."
              : `Mounting task ${Math.round(measured.mountTask ?? 0)} ms with Storybook's render; ${
                  measured.longestTask === 0
                    ? "no task over 50 ms after it"
                    : `longest task after it ${Math.round(measured.longestTask)} ms`
                }.`}
          </span>
        ) : (
          "Measuring…"
        )}
      </p>
      <DiffStory {...props} />
    </div>
  );
}

/**
 * The pull request's diff uses the same view. GitHub sends only hunks, so
 * the path comes from the file entry, and nothing here takes comments.
 */
export const PullRequestFile: Story = {
  render: () => <PullRequestFileStory />,
};

function PullRequestFileStory() {
  const patch = QUEUE_DIFF.split("\n").slice(4).join("\n");
  const group = {
    path: QUEUE_PATH,
    lines: groupUnifiedDiff(patch).flatMap((part) => part.lines),
  };
  const layout = useDiffPreferences((state) => state.layout);
  return (
    <div className="bg-background max-w-3xl rounded-lg border border-border-subtle">
      <p className="border-border-subtle text-muted-foreground border-b px-3 py-2 font-mono text-xs">
        {QUEUE_PATH}
      </p>
      <DiffView group={group} layout={layout} ignoreWhitespace={false} />
    </div>
  );
}
