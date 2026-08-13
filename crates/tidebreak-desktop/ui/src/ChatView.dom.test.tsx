// @vitest-environment jsdom
import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, Chat } from "./api";
import { ChatView, type ChatViewProps } from "./ChatView";
import type { ComposerImages } from "./Composer";
import { useChatSessionStore } from "./ChatSessionStore";
import { useComposerDrafts } from "./ComposerDrafts";
import { usePendingPrompts } from "./PendingPrompts";
import { renderWithRouter } from "./test/router";

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
  listAgentRuns: vi.fn().mockResolvedValue([]),
  getTaskPlan: vi.fn().mockResolvedValue(null),
  decideApproval: vi.fn().mockResolvedValue(undefined),
  cancel: vi.fn().mockResolvedValue(undefined),
  steer: vi.fn().mockResolvedValue(undefined),
  answerUserQuestions: vi.fn().mockResolvedValue(undefined),
} as unknown as ApiClient;

function noImages(): ComposerImages {
  return {
    items: [],
    error: null,
    unsupportedModel: null,
    onAttachFiles: vi.fn(),
    onRemove: vi.fn(),
    onRetry: vi.fn(),
  };
}

function noFiles() {
  return {
    items: [],
    attaching: false,
    onRemove: vi.fn(),
  };
}

/**
 * The pane with the draft wired up the way the root wires it: the store slice
 * the pane subscribes to, and a ref for reading it at the moment guidance is
 * sent.
 */
function DraftingChatView(overrides: Partial<ChatViewProps> = {}) {
  const draftRef = useRef("");
  return (
    <ChatView
      client={client}
      chat={chat}
      hydrated
      nativeHost={false}
      deletingChat={false}
      draftRef={draftRef}
      composerModelMenu={null}
      composerPermissionMenu={null}
      composerImages={noImages()}
      files={noFiles()}
      voiceInputUsed={false}
      onVoiceInputAccepted={vi.fn()}
      attachError={null}
      onDraftChange={(value) => {
        draftRef.current = value;
        useComposerDrafts.getState().setDraft(chat.id, value);
      }}
      onSelectPrompt={vi.fn()}
      onSend={vi.fn(async () => {})}
      {...overrides}
    />
  );
}

function chatViewProps(overrides: Partial<ChatViewProps> = {}): ChatViewProps {
  return {
    client,
    chat,
    hydrated: true,
    nativeHost: false,
    deletingChat: false,
    draftRef: { current: "" },
    composerModelMenu: null,
    composerPermissionMenu: null,
    composerImages: noImages(),
    files: noFiles(),
    voiceInputUsed: false,
    onVoiceInputAccepted: vi.fn(),
    attachError: null,
    onDraftChange: vi.fn(),
    onSelectPrompt: vi.fn(),
    onSend: vi.fn(async () => {}),
    ...overrides,
  };
}

async function renderChatView(overrides: Partial<ChatViewProps> = {}) {
  const props = chatViewProps(overrides);
  await renderWithRouter(<ChatView {...props} />);
  return props;
}

beforeEach(() => {
  useChatSessionStore.getState().reset();
  useComposerDrafts.getState().clearDraft(chat.id);
  usePendingPrompts.setState({ chatId: null, userQuestions: [], folderAccess: [] });
});
afterEach(cleanup);

describe("ChatView", () => {
  it("renders the live session transcript straight from the store", async () => {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      messages: [
        { id: "m1", role: "user", text: "hello there" },
        { id: "m2", role: "assistant", text: "hi!", sources: [] },
      ],
    }));
    await renderChatView();
    expect(screen.getByText("hello there")).toBeInTheDocument();
    expect(screen.getByText("hi!")).toBeInTheDocument();
  });

  it("shows the questions the shell is watching for", async () => {
    // The transcript no longer polls for these — the shell does, so that the
    // agent parking a turn is noticed on any screen. This renders whatever the
    // watcher has published.
    usePendingPrompts.setState({
      chatId: "chat-1",
      userQuestions: [
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
              questionType: "single_select",
              allowFreeForm: true,
            },
          ],
        },
      ] as never,
    });

    await renderChatView();

    expect(await screen.findByText("Which quarter?")).toBeInTheDocument();
  });

  it("stands the question in the composer's place until it is answered", async () => {
    // A parked question is the one thing the turn wants back, so it takes the
    // slot the composer would otherwise fill: nothing to scroll off, and no
    // field inviting a reply the turn will not read.
    const user = userEvent.setup();
    usePendingPrompts.setState({
      chatId: "chat-1",
      userQuestions: [
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
              questionType: "single_select",
              allowFreeForm: true,
            },
          ],
        },
      ] as never,
    });

    await renderChatView();

    expect(await screen.findByText("Which quarter?")).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();

    await user.click(screen.getByRole("button", { name: "Skip questions" }));
    act(() => {
      usePendingPrompts.setState({ chatId: "chat-1", userQuestions: [] });
    });

    // Answered, the slot is the composer's again.
    expect(
      await screen.findByRole("textbox", { name: "Message" }),
    ).toBeInTheDocument();
  });

  it("reveals the card a deep link named, then drops it from the URL", async () => {
    // What the inbox promises: opening an item lands on the exact card that
    // parked, not at the bottom of a transcript the reader has to search.
    usePendingPrompts.setState({
      chatId: "chat-1",
      userQuestions: [
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
              questionType: "single_select",
              allowFreeForm: true,
            },
          ],
        },
      ] as never,
    });
    const scrollIntoView = vi.spyOn(Element.prototype, "scrollIntoView");

    const { router } = await renderWithRouter(
      <ChatView {...chatViewProps()} />,
      { initialUrl: "/c/chat-1?focus=call-q" },
    );

    // The right card, not merely some scroll: the anchor the deep link named
    // is the element that gets pointed out.
    await waitFor(() =>
      expect(
        document.querySelector('[data-pending-call-id="call-q"]'),
      ).toHaveClass("is-deep-linked"),
    );
    expect(scrollIntoView).toHaveBeenCalled();
    // The link is spent once honored: a reload should show the conversation as
    // it stands, not scroll to a card that may long since have been answered.
    await waitFor(() =>
      expect(router.state.location.search).not.toHaveProperty("focus"),
    );
    scrollIntoView.mockRestore();
  });

  it("keeps a transcript anchor in the URL until returning to the live tail", async () => {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      messages: [{ id: "m1", role: "user", text: "Earlier request" }],
    }));
    const user = userEvent.setup();
    const { router } = await renderWithRouter(
      <ChatView {...chatViewProps()} />,
      { initialUrl: "/c/chat-1?at=m1" },
    );

    expect(
      await screen.findByRole("button", { name: "Return to latest" }),
    ).toBeInTheDocument();
    expect(router.state.location.search).toHaveProperty("at", "m1");

    await user.click(screen.getByRole("button", { name: "Return to latest" }));
    await waitFor(() =>
      expect(router.state.location.search).not.toHaveProperty("at"),
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
    await renderChatView();

    const choices = within(
      screen.getByRole("group", { name: "Approval choices" }),
    ).getAllByRole("button");
    await userEvent.click(choices[0]!);

    await waitFor(() =>
      expect(client.decideApproval).toHaveBeenCalledWith(
        "chat-1",
        "call-a",
        "approve",
        null,
      ),
    );
  });

  it("re-renders as stream events land in the store", async () => {
    await renderChatView();
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

  it("does not restart scrolling for every streamed message update", async () => {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      busy: true,
      activeTurnId: "t1",
      messages: [
        { id: "m1", role: "assistant", text: "streamed", sources: [] },
      ],
    }));
    const scrollTo = vi.spyOn(Element.prototype, "scrollTo");
    await renderChatView();
    scrollTo.mockClear();

    act(() => {
      useChatSessionStore.getState().update((session) => ({
        ...session,
        messages: session.messages.map((message) =>
          message.id === "m1" ? { ...message, text: "streamed answer" } : message,
        ),
      }));
    });

    expect(useChatSessionStore.getState().messages[0]).toMatchObject({
      text: "streamed answer",
    });
    expect(scrollTo).not.toHaveBeenCalled();
    scrollTo.mockRestore();
  });

  it("leaves the sent-guidance notice standing when it clears the draft", async () => {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      busy: true,
      activeTurnId: "turn-1",
    }));
    await renderWithRouter(<DraftingChatView />);

    const message = screen.getByLabelText("Message");
    await userEvent.type(message, "go left");
    await userEvent.click(
      screen.getByRole("button", { name: "Steer active response" }),
    );

    // Accepting guidance empties the composer. That clearing must not read as
    // the reader retyping, or the notice they were just given disappears.
    expect(await screen.findByText("Guidance sent")).toBeInTheDocument();
    expect(message).toHaveValue("");

    await userEvent.type(message, "n");

    await waitFor(() =>
      expect(screen.queryByText("Guidance sent")).not.toBeInTheDocument(),
    );
  });

  it("retires the redirect failure the reader is answering by retyping", async () => {
    vi.mocked(client.steer).mockRejectedValueOnce(new Error("steer rejected"));
    useChatSessionStore.getState().update((session) => ({
      ...session,
      busy: true,
      activeTurnId: "turn-1",
    }));
    await renderWithRouter(<DraftingChatView />);

    const message = screen.getByLabelText("Message");
    await userEvent.type(message, "go left");
    await userEvent.click(
      screen.getByRole("button", { name: "Steer active response" }),
    );
    expect(await screen.findByText(/steer rejected/)).toBeInTheDocument();

    await userEvent.type(message, " now");

    await waitFor(() =>
      expect(screen.queryByText(/steer rejected/)).not.toBeInTheDocument(),
    );
  });
});
