import { useCallback, useMemo, useState, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn, userEvent, within } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import {
  MessageList,
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

export const RetryableFailure: Story = {
  args: { messages: failureMessages },
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
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Transcript contents" }),
    );
  },
};
