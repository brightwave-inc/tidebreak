import { useEffect, useRef, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, waitFor, within } from "storybook/test";

import type { ApiClient } from "@/api/client";
import type { CodeWorkspaceDiff } from "@/api/types";
import { DiffPanel } from "@/code/DiffPanel";
import {
  useDiffPreferences,
  type DiffLayout,
} from "@/code/diff/diffPreferences";
import { DiffView } from "@/code/diff/DiffView";
import { usePendingReviewStore } from "@/code/diff/pendingReview";
import type {
  ReviewComment,
  ReviewCommentLine,
} from "@/code/diff/reviewComments";
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
function DiffStory({ diff, path, workspaceId, height = 560 }: DiffStoryProps) {
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
    const store = usePendingReviewStore.getState();
    store.finishSend(workspaceId, store.sending[workspaceId] ?? [], false);
    store.clear(workspaceId);
    for (const item of comments) store.add(workspaceId, item);
    if (sending.length > 0) store.beginSend(workspaceId, sending);
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
 * One was written on a line this diff no longer shows, so it keeps its
 * quote at the top.
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
 * the rest follow a few chunks a frame, so opening it never holds the main
 * thread. The caption reports what this browser measured.
 */
export const VeryLongFile: Story = {
  args: {
    diff: longFileDiff(5_000),
    path: GENERATED_PATH,
    workspaceId: "ws-diff-long",
  },
  loaders: [seedReview("ws-diff-long", [])],
  render: (args) => <LongFileStory {...args} />,
  play: async ({ canvasElement }) => {
    await waitFor(
      () =>
        expect(
          canvasElement.querySelector("[data-long-diff-measured]"),
        ).not.toBeNull(),
      { timeout: 20_000 },
    );
  },
};

type Measurement = {
  lines: number;
  firstRows: number;
  allRows: number;
  longestTask: number | null;
};

function LongFileStory(props: DiffStoryProps) {
  const started = useRef(performance.now());
  const [measured, setMeasured] = useState<Measurement | null>(null);
  const firstRows = useRef<number | null>(null);
  const longest = useRef<number | null>(null);
  const expected = groupUnifiedDiff(props.diff)[0]?.lines.length ?? 0;

  useEffect(() => {
    let observer: PerformanceObserver | null = null;
    try {
      observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          // Storybook's own start-up is not this diff's work.
          if (entry.startTime + entry.duration < started.current) continue;
          longest.current = Math.max(longest.current ?? 0, entry.duration);
        }
      });
      observer.observe({ type: "longtask", buffered: true });
      longest.current = 0;
    } catch {
      // Not every engine reports long tasks; the caption says so.
    }
    let frame = 0;
    const poll = () => {
      const rows = document.querySelectorAll("[data-diff-view] [data-row]");
      if (rows.length > 0 && firstRows.current === null) {
        firstRows.current = performance.now() - started.current;
      }
      if (rows.length >= expected - 1) {
        setMeasured({
          lines: rows.length,
          firstRows: firstRows.current ?? 0,
          allRows: performance.now() - started.current,
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
  }, [expected]);

  return (
    <div className="flex flex-col gap-2">
      <p className="text-muted-foreground text-xs" role="status">
        {measured ? (
          <span data-long-diff-measured="">
            {measured.lines.toLocaleString()} lines: first rows in{" "}
            {Math.round(measured.firstRows)} ms, every row in{" "}
            {Math.round(measured.allRows)} ms,{" "}
            {measured.longestTask === null
              ? "long tasks not reported by this browser"
              : measured.longestTask === 0
                ? "no main-thread task over 50 ms"
                : `longest main-thread task ${Math.round(measured.longestTask)} ms`}
            .
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
