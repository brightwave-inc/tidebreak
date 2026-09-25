import { useCallback, useMemo, useState, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor, within } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import type {
  AnswerVersions,
  LatestTurnSideEffects,
} from "@/ChatTranscriptPresentation";
import type { TurnActions } from "@/MessageActions";
import {
  MessageList,
  TRANSCRIPT_TURNS_SHOWN,
  TURN_CANCELLED_NOTICE,
  type BranchOrigin,
  type ChatMessage,
  type RetryableTurn,
} from "@/MessageList";
import {
  TranscriptNavigation,
  transcriptNavigationEntries,
} from "@/TranscriptNavigation";

type ConversationTranscriptProps = {
  messages: ChatMessage[];
  busy?: boolean;
  hydrated?: boolean;
  compacting?: boolean;
  streamStalled?: boolean;
  pinLastTurn?: boolean;
  onRetryTurn?: (turn: RetryableTurn) => void;
  hasEarlierMessages?: boolean;
  onLoadEarlierMessages?: () => Promise<void>;
  turnActions?: TurnActions;
  answerVersions?: AnswerVersions;
  latestSideEffects?: LatestTurnSideEffects | null;
  branchOrigin?: BranchOrigin;
};

function withRouter(children: ReactNode) {
  const rootRoute = createRootRoute({ component: () => children });
  const router = createRouter({
    routeTree: rootRoute,
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return <RouterProvider router={router as never} />;
}

function ConversationTranscript({
  messages,
  busy = false,
  hydrated = true,
  compacting = false,
  streamStalled = false,
  pinLastTurn = false,
  onRetryTurn,
  hasEarlierMessages,
  onLoadEarlierMessages,
  turnActions,
  answerVersions,
  latestSideEffects,
  branchOrigin,
}: ConversationTranscriptProps) {
  const [scrollElement, setScrollElement] = useState<HTMLDivElement | null>(
    null,
  );
  const [activeAnchor, setActiveAnchor] = useState<string>();
  const navigationEntries = useMemo(
    () => transcriptNavigationEntries(messages),
    [messages],
  );
  const attachScrollRef = useCallback((node: HTMLDivElement | null) => {
    setScrollElement(node);
  }, []);
  const jumpToMessage = useCallback(
    (anchorId: string) => {
      setActiveAnchor(anchorId);
      const target = scrollElement
        ? Array.from(
            scrollElement.querySelectorAll<HTMLElement>(
              "[data-transcript-anchor]",
            ),
          ).find((element) => element.dataset.transcriptAnchor === anchorId)
        : null;
      target?.scrollIntoView({ block: "start", behavior: "smooth" });
    },
    [scrollElement],
  );

  return (
    <section className="chat-pane mx-auto h-full w-full max-w-5xl overflow-hidden border-x border-border bg-background">
      <div className="message-view">
        <MessageList
          messages={messages}
          folderAccessRequests={[]}
          nativeHost={false}
          nativeBusy={false}
          resolvingFolderCalls={new Set()}
          folderAccessErrors={{}}
          decidingApprovalCalls={new Set()}
          approvalErrors={{}}
          busy={busy}
          hydrated={hydrated}
          compacting={compacting}
          streamStalled={streamStalled}
          pinLastTurn={pinLastTurn}
          scrollRef={attachScrollRef}
          onScroll={fn()}
          onApproval={fn()}
          onFolderAccessDecision={fn()}
          onFolderAccessCancel={fn()}
          onSelectPrompt={fn()}
          onRetryTurn={onRetryTurn}
          hasEarlierMessages={hasEarlierMessages}
          onLoadEarlierMessages={onLoadEarlierMessages}
          turnActions={turnActions}
          answerVersions={answerVersions}
          latestSideEffects={latestSideEffects}
          branchOrigin={branchOrigin}
        />
        <TranscriptNavigation
          entries={navigationEntries}
          scrollElement={scrollElement}
          activeAnchor={activeAnchor}
          onJump={jumpToMessage}
        />
      </div>
    </section>
  );
}

const sourceA = {
  id: "source-a",
  ordinal: 1,
  documentId: "design-audit",
  locator: { kind: "pages", start: 8, end: 10 } as const,
};

const sourceB = {
  id: "source-b",
  ordinal: 2,
  documentId: "accessibility-notes",
  locator: { kind: "page", page: 4 } as const,
};

const streamingMessages: ChatMessage[] = [
  {
    id: "stream-user",
    role: "user",
    text: "Audit the conversation experience and explain the highest-impact improvements.",
    createdAt: "2026-08-24T14:02:00.000Z",
  },
  {
    id: "stream-assistant",
    role: "assistant",
    reasoning:
      "I am comparing the dense tool phase with the compact transcript and checking which details should stay collapsed.",
    text: "The main hierarchy problem is that long-running work gives each activity equal visual weight. I am grouping routine tool calls and keeping decisions visible.",
    sources: [],
    createdAt: "2026-08-24T14:02:08.000Z",
  },
];

const streamingReasoningMessages: ChatMessage[] = [
  streamingMessages[0],
  {
    id: "stream-reasoning",
    role: "assistant",
    reasoning:
      "I am comparing the dense tool phase with the compact transcript and checking which details should stay collapsed.",
    text: "",
    sources: [],
    createdAt: "2026-08-24T14:02:06.000Z",
  },
];

const runningToolMessages: ChatMessage[] = [
  {
    id: "run-user",
    role: "user",
    text: "Review the current Storybook coverage, run the focused checks, and summarize the gaps.",
    createdAt: "2026-08-24T14:10:00.000Z",
  },
  {
    id: "run-search",
    role: "tool",
    callId: "call-search",
    name: "search",
    status: "running",
    preview: {
      tool: "search",
      query: "title: Conversation/ OR title: Composer/ OR title: Chat/",
    },
  },
];

const toolHeavyMessages: ChatMessage[] = [
  {
    id: "tools-user",
    role: "user",
    text: "Review the current Storybook coverage, run the focused checks, and summarize the gaps.",
    createdAt: "2026-08-24T14:10:00.000Z",
  },
  {
    id: "tools-search",
    role: "tool",
    callId: "call-search",
    name: "search",
    status: "completed",
    preview: {
      tool: "search",
      query: "title: Conversation/ OR title: Composer/ OR title: Chat/",
    },
  },
  {
    id: "tools-read",
    role: "tool",
    callId: "call-read",
    name: "read_file",
    status: "completed",
  },
  {
    id: "tools-exec",
    role: "tool",
    callId: "call-exec",
    name: "exec",
    status: "completed",
    preview: {
      tool: "exec",
      command: "pnpm",
      args: ["exec", "biome", "check", "src/stories"],
      cwd: "crates/tidebreak-desktop/ui",
      files: [],
    },
    result: {
      tool: "exec",
      exitCode: 0,
      timedOut: false,
      outputTruncated: false,
      stdout: "Checked 12 files in 41ms. No fixes applied.\n",
      stderr: "",
      backend: "local",
    },
  },
  {
    id: "tools-assistant",
    role: "assistant",
    text: "The focused checks pass. The largest coverage gap is the transcript as a system: empty, loading, streaming, tool-heavy, failure, and compact states were not visible together.",
    sources: [sourceA],
    createdAt: "2026-08-24T14:10:18.000Z",
  },
];

/**
 * Prose, a quiet success, a live bounded run, and a failed excerpt in one
 * column — the mixed journal the tool-row redesign is judged against.
 */
const mixedExecMessages: ChatMessage[] = [
  {
    id: "mixed-user",
    role: "user",
    text: "Run the formatter, start the unit suite, and fix the deck renderer if it fails.",
    createdAt: "2026-09-09T15:00:00.000Z",
  },
  {
    id: "mixed-assistant-1",
    role: "assistant",
    text: "I will keep routine commands quiet in the journal and only open failures so the error is one glance away.",
    sources: [],
    createdAt: "2026-09-09T15:00:04.000Z",
  },
  {
    id: "mixed-exec-ok",
    role: "tool",
    callId: "mixed-ok",
    name: "exec",
    status: "completed",
    preview: {
      tool: "exec",
      command: "pnpm",
      args: ["exec", "biome", "check", "src/ToolCallCard.tsx"],
      cwd: "crates/tidebreak-desktop/ui",
      files: [],
      summary: "Checking the tool card module",
    },
    result: {
      tool: "exec",
      exitCode: 0,
      timedOut: false,
      outputTruncated: false,
      stdout: "Checked 1 file in 18ms. No fixes applied.\n",
      stderr: "",
      backend: "local",
    },
  },
  {
    id: "mixed-exec-run",
    role: "tool",
    callId: "mixed-run",
    name: "exec",
    status: "running",
    preview: {
      tool: "exec",
      command: "pnpm",
      args: ["test", "src/ToolPreview.test.ts"],
      cwd: "crates/tidebreak-desktop/ui",
      files: [],
      summary: "Running the preview unit tests",
    },
    result: {
      tool: "exec",
      exitCode: null,
      timedOut: false,
      outputTruncated: false,
      stdout: Array.from(
        { length: 16 },
        (_, index) => ` ✓ grounded headline case ${index + 1}`,
      ).join("\n"),
      stderr: "",
      backend: "local",
    },
  },
  {
    id: "mixed-exec-fail",
    role: "tool",
    callId: "mixed-fail",
    name: "exec",
    status: "failed",
    preview: {
      tool: "exec",
      command: "python3",
      args: [
        "/workspace/checkout/scripts/very/long/path/render_deck.py",
        "reports/q3.pptx",
      ],
      cwd: "checkout",
      files: ["reports/q3.pptx"],
    },
    result: {
      tool: "exec",
      exitCode: 1,
      timedOut: false,
      outputTruncated: false,
      stdout: "",
      stderr:
        "Error: Cannot find module 'pptxgenjs'\n    at Object.<anonymous> (scripts/render_deck.py:12)\n",
      backend: "local",
    },
  },
  {
    id: "mixed-assistant-2",
    role: "assistant",
    text: "The deck renderer is missing `pptxgenjs`. I can install it next, or switch the script to the workspace package that already provides it.",
    sources: [],
    createdAt: "2026-09-09T15:00:40.000Z",
  },
];

const failureMessages: ChatMessage[] = [
  {
    id: "failure-user",
    role: "user",
    text: "Generate the screenshots for every dense conversation state.",
    invokedSkills: ["browser"],
    createdAt: "2026-08-24T14:18:00.000Z",
  },
  {
    id: "failure-turn",
    role: "turn_failure",
    category: "transient",
    detail: "The provider closed the stream before the response completed.",
    model: { id: "gpt-5.6-sol", provider: "model_gateway" },
    invokedSkills: ["browser"],
  },
];

const blockedMessages: ChatMessage[] = [
  {
    id: "blocked-user",
    role: "user",
    text: "Run the required browser smoke check and save the results.",
    invokedSkills: ["browser"],
    createdAt: "2026-09-03T04:44:00.000Z",
  },
  {
    id: "blocked-assistant",
    role: "assistant",
    text: "No browser is connected to this Tidebreak profile, so I could not run the required smoke check or save its results.",
    sources: [],
    createdAt: "2026-09-03T04:45:00.000Z",
  },
  {
    id: "blocked-refusal",
    role: "refusal",
    category: "blocked",
    partialOutput: true,
    source: "report_blocked",
  },
];

const denseMessages: ChatMessage[] = [
  {
    id: "dense-user-1",
    role: "user",
    text: "Map the conversation surfaces before changing them.",
    files: [
      {
        documentId: "contract",
        name: "storybook-review-contract.md",
        mediaType: "text/markdown",
      },
    ],
    invokedSkills: ["redesign-existing-projects", "browser"],
    createdAt: "2026-08-24T13:20:00.000Z",
  },
  {
    id: "dense-assistant-1",
    role: "assistant",
    text: "The transcript has strong isolated cards, but no composed story shows how those cards compete for attention during a long session.",
    sources: [sourceA, sourceB],
    createdAt: "2026-08-24T13:20:12.000Z",
  },
  { id: "dense-compaction", role: "compaction" },
  {
    id: "dense-user-2",
    role: "user",
    text: "Add the missing states and keep routine activity quiet.",
    createdAt: "2026-08-24T13:36:00.000Z",
  },
  {
    id: "dense-tool-1",
    role: "tool",
    callId: "dense-call-1",
    name: "search",
    status: "completed",
    preview: { tool: "search", query: "conversation components" },
  },
  {
    id: "dense-tool-2",
    role: "tool",
    callId: "dense-call-2",
    name: "read_file",
    status: "completed",
  },
  {
    id: "dense-assistant-2",
    role: "assistant",
    text: "Routine reads and searches now collapse into one activity phase. Decisions, failures, and published results remain visible because they change what you do next.",
    sources: [],
    createdAt: "2026-08-24T13:36:26.000Z",
  },
  {
    id: "dense-user-3",
    role: "user",
    text: "Check the compact pane and call out any remaining limitation.",
    createdAt: "2026-08-24T13:44:00.000Z",
  },
  {
    id: "dense-refusal",
    role: "refusal",
    category: "general_harms",
    partialOutput: false,
  },
  {
    id: "dense-user-4",
    role: "user",
    text: "Continue with the UI review only.",
    createdAt: "2026-08-24T13:46:00.000Z",
  },
  {
    id: "dense-assistant-3",
    role: "assistant",
    reasoning:
      "The compact pane keeps the same order, but I am checking whether tool labels and source pills still leave enough room for the answer.",
    text: "The compact pane remains readable. The transcript rail hides below the desktop breakpoint, and the contents menu keeps navigation available without taking horizontal space.",
    sources: [sourceB],
    createdAt: "2026-08-24T13:46:19.000Z",
  },
];

const meta = {
  title: "Conversation/Transcript",
  component: ConversationTranscript,
  parameters: { layout: "fullscreen" },
  args: {
    messages: toolHeavyMessages,
    busy: false,
    hydrated: true,
    compacting: false,
    streamStalled: false,
    pinLastTurn: false,
    onRetryTurn: fn(),
  },
  decorators: [
    (Story) => (
      <div className="h-screen min-h-0 bg-page-background">
        {withRouter(<Story />)}
      </div>
    ),
  ],
} satisfies Meta<typeof ConversationTranscript>;

export default meta;
type Story = StoryObj<typeof meta>;

export const NewConversation: Story = {
  args: { messages: [] },
};

export const LoadingHistory: Story = {
  args: { messages: [], hydrated: false },
};

export const StreamingResponse: Story = {
  args: { messages: streamingMessages, busy: true, pinLastTurn: true },
};

/** Reasoning is still arriving, so Thinking shimmers and the body stays open. */
export const StreamingReasoning: Story = {
  args: {
    messages: streamingReasoningMessages,
    busy: true,
    pinLastTurn: true,
  },
};

/** A live tool phase shimmers on the folded line until the call settles. */
export const RunningTools: Story = {
  args: { messages: runningToolMessages, busy: true, pinLastTurn: true },
};

export const StalledStream: Story = {
  args: {
    messages: streamingMessages,
    busy: true,
    streamStalled: true,
    pinLastTurn: true,
  },
};

export const CompactingConversation: Story = {
  args: { messages: toolHeavyMessages, busy: true, compacting: true },
};

export const ToolHeavySession: Story = {};

/**
 * Mixed journal: prose, quiet success, bounded running output, and a failed
 * command with an error excerpt — all in the shared reading column.
 */
export const MixedToolRows: Story = {
  args: { messages: mixedExecMessages, busy: true, pinLastTurn: true },
};

/** Same mix at a narrow pane width so truncation cannot widen the column. */
export const MixedToolRowsNarrow: Story = {
  args: { messages: mixedExecMessages, busy: true, pinLastTurn: true },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const RetryableFailure: Story = {
  args: { messages: failureMessages },
};

/** An operational blocker stays distinct from a provider safety refusal. */
export const BlockedTurn: Story = {
  args: { messages: blockedMessages },
};

export const DenseLongRunningSession: Story = {
  args: { messages: denseMessages },
};

export const CompactWidth: Story = {
  args: { messages: denseMessages },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const TranscriptContents: Story = {
  args: { messages: denseMessages },
};

export const Notices: Story = {
  args: {
    messages: [
      { id: "notice-system", role: "system", text: "Turn stopped." },
      {
        id: "notice-error",
        role: "error",
        text: "The connection closed before the response completed. Send the message again to continue.",
      },
      blockedMessages[2],
      { id: "notice-compaction", role: "compaction" },
      ...failureMessages,
    ],
  },
  play: async ({ canvasElement }) => {
    const notices = () =>
      Array.from(
        canvasElement.querySelectorAll<HTMLElement>('[data-slot="notice"]'),
      );
    // The transcript draws its rows after it measures the column.
    await waitFor(() => expect(notices().length).toBeGreaterThan(2));
    const first = notices()[0].getBoundingClientRect();
    for (const notice of notices()) {
      const rect = notice.getBoundingClientRect();
      await expect(Math.abs(rect.width - first.width)).toBeLessThan(1);
      await expect(Math.abs(rect.left - first.left)).toBeLessThan(1);
      await expect(notice.scrollWidth).toBeLessThanOrEqual(notice.clientWidth);
    }
  },
};

/**
 * `count` turns of the audit exchange above, each with its own ids, oldest
 * first — enough history for the transcript to open on its newest turns.
 */
function longConversation(count: number): ChatMessage[] {
  const [question, answer] = [streamingMessages[0]!, denseMessages.at(-1)!];
  return Array.from({ length: count }, (_, turn) => [
    { ...question, id: `long-user-${turn}` },
    { ...answer, id: `long-assistant-${turn}` },
  ]).flat();
}

/** The transcript renders after the story mounts, so wait for the control. */
const earlierButton = (canvasElement: HTMLElement) =>
  within(canvasElement).findByRole("button", { name: "Show earlier messages" });

/**
 * A long conversation opens on its newest turns. The control at the top
 * reveals the turns already held before it fetches older ones.
 */
export const EarlierTurnsHeld: Story = {
  args: { messages: longConversation(TRANSCRIPT_TURNS_SHOWN + 4) },
  play: async ({ canvasElement }) => {
    await expect(await earlierButton(canvasElement)).toBeEnabled();
  },
};

/** Every held turn is shown, and the page before them is on its way. */
export const EarlierPageLoading: Story = {
  args: {
    messages: longConversation(3),
    hasEarlierMessages: true,
    onLoadEarlierMessages: () => new Promise<void>(() => undefined),
  },
  play: async ({ canvasElement }) => {
    await userEvent.click(await earlierButton(canvasElement));
    await expect(await earlierButton(canvasElement)).toBeDisabled();
  },
};

/** The earlier page did not arrive; the control stays to try again. */
export const EarlierPageFailed: Story = {
  args: {
    messages: longConversation(3),
    hasEarlierMessages: true,
    onLoadEarlierMessages: () => Promise.reject(new Error("offline")),
  },
  play: async ({ canvasElement }) => {
    await userEvent.click(await earlierButton(canvasElement));
    await expect(
      await within(canvasElement).findByRole("alert"),
    ).toHaveTextContent("Could not load earlier messages");
  },
};

/**
 * A settled two-turn exchange, the shape every message action works on. Each
 * message names its turn, which is what an action sends back.
 */
const actionMessages: ChatMessage[] = [
  {
    id: "actions-user-1",
    role: "user",
    turnId: "actions-turn-1",
    text: "Draft a two-day coastal walk from Porthleven to Mousehole.",
    createdAt: "2026-09-20T09:30:00.000Z",
  },
  {
    id: "actions-assistant-1",
    role: "assistant",
    turnId: "actions-turn-1",
    text: "**Day one** runs from Porthleven to Marazion along the cliffs, about 12 miles. **Day two** crosses to Penzance and follows the bay to Mousehole, about 8 miles.",
    sources: [],
    createdAt: "2026-09-20T09:30:14.000Z",
  },
  {
    id: "actions-user-2",
    role: "user",
    turnId: "actions-turn-2",
    text: "Make day two shorter and end somewhere with a café.",
    createdAt: "2026-09-20T09:31:00.000Z",
  },
  {
    id: "actions-assistant-2",
    role: "assistant",
    turnId: "actions-turn-2",
    text: "Day two now stops at Newlyn after 5 miles. The harbor has two cafés that open early, and the bus back to Penzance leaves every half hour.",
    sources: [],
    createdAt: "2026-09-20T09:31:12.000Z",
  },
];

const storyTurnActions: TurnActions = {
  onRegenerate: fn(),
  onEdit: fn(),
  onBranch: fn(),
  retryModels: [
    {
      label: "Anthropic",
      models: [
        { key: "anthropic::claude-opus-5", label: "Claude Opus 5" },
        { key: "anthropic::claude-sonnet-5", label: "Claude Sonnet 5" },
      ],
    },
    {
      label: "OpenAI",
      models: [{ key: "openai::gpt-5.6", label: "GPT-5.6" }],
    },
  ],
  currentModelKey: "anthropic::claude-opus-5",
  pending: false,
};

/** Two earlier answers to the latest message, oldest first. */
const earlierAnswers: AnswerVersions = {
  "actions-turn-2": [
    {
      turnId: "actions-turn-2a",
      messages: [
        {
          id: "actions-version-a",
          role: "assistant",
          turnId: "actions-turn-2a",
          text: "Day two now ends in Penzance after 4 miles, at the promenade café.",
          sources: [],
          createdAt: "2026-09-20T09:30:40.000Z",
        },
      ],
    },
    {
      turnId: "actions-turn-2b",
      messages: [
        {
          id: "actions-version-b",
          role: "assistant",
          turnId: "actions-turn-2b",
          text: "Day two stops at Newlyn fish market; the café there serves lunch from 11.",
          sources: [],
          createdAt: "2026-09-20T09:30:55.000Z",
        },
      ],
    },
  ],
};

/**
 * The latest answer keeps its actions in view: Copy, Regenerate with Retry
 * with model beside it, Branch from here. Older messages show theirs on hover
 * and keyboard focus.
 */
export const MessageActions: Story = {
  args: { messages: actionMessages, turnActions: storyTurnActions },
};

/** Keyboard focus reveals an older answer's actions, and the question's. */
export const MessageActionsOnFocus: Story = {
  args: { messages: actionMessages, turnActions: storyTurnActions },
  play: async ({ canvasElement }) => {
    const branches = await within(canvasElement).findAllByRole("button", {
      name: "Branch from here",
    });
    branches[0].focus();
    await expect(branches[0]).toHaveFocus();
  },
};

/** Edit opens the latest message in place of its bubble. */
export const EditingMessage: Story = {
  args: { messages: actionMessages, turnActions: storyTurnActions },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByRole("button", { name: "Edit" }),
    );
    const field = within(canvasElement).getByRole("textbox", {
      name: "Message",
    });
    await userEvent.clear(field);
    await userEvent.type(
      field,
      "Make day two shorter, end somewhere with a café, and keep the walk on the coast path the whole way.",
    );
  },
};

/**
 * The answer being replaced wrote files and used a connected app, so the edit
 * says before sending that it starts a new conversation.
 */
export const EditThatStartsNewChat: Story = {
  args: {
    messages: actionMessages,
    turnActions: storyTurnActions,
    latestSideEffects: {
      turnId: "actions-turn-2",
      effects: ["files_written", "connected_apps_called"],
    },
  },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByRole("button", { name: "Edit" }),
    );
    await userEvent.type(
      within(canvasElement).getByRole("textbox", { name: "Message" }),
      " Leave the files as they are.",
    );
  },
};

/** A regenerated answer pages back through the answers before it. */
export const RegeneratedAnswerVersions: Story = {
  args: {
    messages: actionMessages,
    turnActions: storyTurnActions,
    answerVersions: earlierAnswers,
  },
};

/** Paging back shows an earlier answer in place; the question stays. */
export const EarlierAnswerVersion: Story = {
  args: {
    messages: actionMessages,
    turnActions: storyTurnActions,
    answerVersions: earlierAnswers,
  },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByRole("button", {
        name: "Previous version",
      }),
    );
    await expect(within(canvasElement).getByText("2 of 3")).toBeInTheDocument();
  },
};

/** "Retry with model" lists the models the chat can run. */
export const RetryWithModel: Story = {
  args: { messages: actionMessages, turnActions: storyTurnActions },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByRole("button", {
        name: "Retry with model…",
      }),
    );
  },
};

/**
 * The answer on screen ran commands, so Regenerate says before sending that
 * the new answer starts a new conversation, and this one stays as it is.
 */
export const RegenerateThatStartsNewChat: Story = {
  args: {
    messages: actionMessages,
    turnActions: storyTurnActions,
    latestSideEffects: {
      turnId: "actions-turn-2",
      effects: ["commands_run"],
    },
  },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByRole("button", {
        name: "Regenerate",
      }),
    );
  },
};

/** A branch's copied history ends at a rule that links back to its original. */
export const BranchNotice: Story = {
  args: {
    messages: [
      ...actionMessages.slice(0, 2),
      {
        id: "branch-user",
        role: "user",
        turnId: "branch-turn",
        text: "Try the north coast instead: St Ives to Zennor.",
        createdAt: "2026-09-21T08:00:00.000Z",
      },
      {
        id: "branch-assistant",
        role: "assistant",
        turnId: "branch-turn",
        text: "St Ives to Zennor is about 6 miles of rough path. Start early; the café in Zennor closes at 4.",
        sources: [],
        createdAt: "2026-09-21T08:00:15.000Z",
      },
    ],
    turnActions: storyTurnActions,
    branchOrigin: {
      title: "Coastal walk planning",
      branchedAt: "2026-09-21T07:59:00.000Z",
      onOpen: fn(),
    },
  },
};

/** A branch whose original was deleted still says where it came from. */
export const BranchOfDeletedChat: Story = {
  args: {
    messages: actionMessages.slice(0, 2),
    turnActions: storyTurnActions,
    branchOrigin: {
      title: null,
      branchedAt: "2026-09-21T07:59:00.000Z",
    },
  },
};

/** A stopped answer offers to answer again, in place. */
export const RetryAfterCancel: Story = {
  args: {
    messages: [
      ...actionMessages.slice(0, 3),
      {
        id: "cancel-partial",
        role: "assistant",
        turnId: "actions-turn-2",
        text: "Day two now stops at Newlyn after 5 miles. The harbor",
        sources: [],
        createdAt: "2026-09-20T09:31:06.000Z",
      },
      {
        id: "cancel-notice",
        role: "system",
        turnId: "actions-turn-2",
        text: TURN_CANCELLED_NOTICE,
      },
    ],
    turnActions: storyTurnActions,
  },
};

/**
 * The provider failed after the answer had already sent an invoice. The call
 * stays in view, and Try again continues from it rather than starting over.
 */
export const RetryAfterAToolCall: Story = {
  args: {
    messages: [
      ...actionMessages.slice(0, 3),
      {
        id: "invoice-call",
        role: "tool",
        callId: "call-invoice",
        name: "mcp__billing__send_invoice",
        status: "completed",
      },
      {
        id: "overloaded-failure",
        role: "turn_failure",
        turnId: "actions-turn-2",
        category: "transient",
        detail: "overloaded_error: Overloaded",
        model: { id: "claude-opus-5", provider: "anthropic" },
      },
    ],
    turnActions: storyTurnActions,
  },
};

/**
 * A rejected key points at provider settings, with Try again beside it for
 * after the fix. The retry continues the same turn; the question shows once.
 */
export const RetryAfterProviderError: Story = {
  args: {
    messages: [
      ...actionMessages.slice(0, 3),
      {
        id: "auth-failure",
        role: "turn_failure",
        turnId: "actions-turn-2",
        category: "auth",
        detail: "invalid x-api-key",
        model: { id: "claude-opus-5", provider: "anthropic" },
      },
    ],
    turnActions: storyTurnActions,
  },
};

/** Account access denied: the same pair of recoveries. */
export const RetryAfterProviderAccessDenied: Story = {
  args: {
    messages: [
      ...actionMessages.slice(0, 3),
      {
        id: "access-failure",
        role: "turn_failure",
        turnId: "actions-turn-2",
        category: "provider_access",
        detail: "Your credit balance is too low to access the API.",
        model: { id: "gpt-5.6", provider: "openai" },
      },
    ],
    turnActions: storyTurnActions,
  },
};

/** The same actions and pager at the compact pane width. */
export const MessageActionsCompact: Story = {
  args: {
    messages: actionMessages,
    turnActions: storyTurnActions,
    answerVersions: earlierAnswers,
  },
  globals: { viewport: { value: "compact", isRotated: false } },
};
