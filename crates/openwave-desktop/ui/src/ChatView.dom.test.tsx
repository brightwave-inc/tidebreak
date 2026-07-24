// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, Chat } from "./api";
import { ChatView, type ChatViewProps } from "./ChatView";
import { useChatSessionStore } from "./ChatSessionStore";

vi.mock("./host", () => ({
  hasNativeHost: () => false,
  requestUserAttention: vi.fn().mockResolvedValue(undefined),
  resolveFolderAccessRequest: vi.fn(),
}));

const chat = { id: "chat-1", title: "Roadmap", project_id: null } as unknown as Chat;

// The chat pane owns its conversation's request state, so it polls through the
// client rather than being handed the results.
const client = {
  listPendingFolderAccessRequests: vi.fn().mockResolvedValue([]),
  listPendingUserQuestions: vi.fn().mockResolvedValue([]),
  decideApproval: vi.fn().mockResolvedValue(undefined),
  cancel: vi.fn().mockResolvedValue(undefined),
  answerUserQuestions: vi.fn().mockResolvedValue(undefined),
} as unknown as ApiClient;

function renderChatView(overrides: Partial<ChatViewProps> = {}) {
  const props: ChatViewProps = {
    client,
    chat,
    hydrated: true,
    nativeHost: false,
    deletingChat: false,
    agentRuns: [],
    agentRunsLoading: false,
    agentRunsError: null,
    stoppingRunIds: new Set(),
    stopErrorRunIds: new Set(),
    onRetryAgentRuns: vi.fn(),
    onStopSandboxRun: vi.fn(),
    draft: "",
    composerModelMenu: null,
    attachingSource: false,
    attachedSourceName: null,
    sourceAttachmentError: null,
    cancelError: null,
    cancelPendingTurnId: null,
    steerError: null,
    steerStatus: null,
    steerPendingTurnId: null,
    onDraftChange: vi.fn(),
    onAddSource: vi.fn(async () => {}),
    onDismissAttachedSource: vi.fn(),
    onSelectPrompt: vi.fn(),
    onSend: vi.fn(async () => {}),
    onSteer: vi.fn(async () => {}),
    onStop: vi.fn(async () => {}),
    ...overrides,
  };
  render(<ChatView {...props} />);
  return props;
}

beforeEach(() => {
  useChatSessionStore.getState().reset();
});
afterEach(cleanup);

describe("ChatView", () => {
  it("renders the live session transcript straight from the store", () => {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      messages: [
        { id: "m1", role: "user", text: "hello there" },
        { id: "m2", role: "assistant", text: "hi!", sources: [] },
      ],
    }));
    renderChatView();
    expect(screen.getByText("hello there")).toBeInTheDocument();
    expect(screen.getByText("hi!")).toBeInTheDocument();
  });

  it("polls for its own conversation's pending approvals and questions", async () => {
    vi.mocked(client.listPendingUserQuestions).mockResolvedValueOnce([
      {
        callId: "call-q",
        turnId: "turn-1",
        askedAt: "2026-07-24T00:00:00.000Z",
        questions: [
          {
            id: "q1",
            header: "Scope",
            question: "Which quarter?",
            options: [],
            allowFreeForm: true,
          },
        ],
      },
    ]);

    renderChatView();

    expect(await screen.findByText("Which quarter?")).toBeInTheDocument();
    expect(client.listPendingUserQuestions).toHaveBeenCalledWith("chat-1");
    expect(client.listPendingFolderAccessRequests).toHaveBeenCalledWith(
      "chat-1",
    );
  });

  it("sends an approval decision through its own client", async () => {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      messages: [
        {
          id: "m1",
          role: "approval",
          callId: "call-a",
          summary: "Run a command",
          canApprove: true,
          canRemember: false,
        },
      ],
    }));
    renderChatView();

    await userEvent.click(screen.getAllByRole("option")[0]!);

    await waitFor(() =>
      expect(client.decideApproval).toHaveBeenCalledWith(
        "chat-1",
        "call-a",
        "approve",
        false,
      ),
    );
  });

  it("re-renders as stream events land in the store", () => {
    renderChatView();
    act(() => {
      useChatSessionStore
        .getState()
        .applyEvent(
          { seq: 1, event: { type: "turn_started", turn_id: "t1" } },
          { nextId: () => "m1", now: () => "2026-07-23T12:00:00.000Z" },
        );
      useChatSessionStore
        .getState()
        .applyEvent(
          { seq: 2, event: { type: "text_delta", text: "streamed answer" } },
          { nextId: () => "m2", now: () => "2026-07-23T12:00:00.000Z" },
        );
    });
    expect(screen.getByText("streamed answer")).toBeInTheDocument();
  });
});
