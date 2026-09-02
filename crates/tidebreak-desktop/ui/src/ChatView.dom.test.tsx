// @vitest-environment jsdom
import {
  act,
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
  useNavigate,
  useParams,
  useRouter,
} from "@tanstack/react-router";
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

const chat = {
  id: "chat-1",
  title: "Roadmap",
  project_id: null,
} as unknown as Chat;

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
 * The pane with the draft wired up through the store slice it subscribes to.
 */
function DraftingChatView(overrides: Partial<ChatViewProps> = {}) {
  return (
    <ChatView
      client={client}
      chat={chat}
      hydrated
      nativeHost={false}
      deletingChat={false}
      composerModelMenu={null}
      composerPermissionMenu={null}
      composerImages={noImages()}
      files={noFiles()}
      voiceInputUsed={false}
      onVoiceInputAccepted={vi.fn()}
      attachError={null}
      onDraftChange={(value) =>
        useComposerDrafts.getState().setDraft(chat.id, value)
      }
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

const otherChat = {
  id: "chat-2",
  title: "Other work",
  project_id: null,
} as unknown as Chat;

beforeEach(() => {
  useChatSessionStore.getState().reset();
  useComposerDrafts.getState().clearDraft(chat.id);
  useComposerDrafts.getState().clearDraft(otherChat.id);
  window.sessionStorage.clear();
  usePendingPrompts.setState({
    chatId: null,
    userQuestions: [],
    folderAccess: [],
  });
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
          message.id === "m1"
            ? { ...message, text: "streamed answer" }
            : message,
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

  it("sends and clears pasted text used as guidance", async () => {
    vi.mocked(client.steer).mockResolvedValue(undefined);
    useChatSessionStore.getState().update((session) => ({
      ...session,
      busy: true,
      activeTurnId: "turn-1",
    }));
    useComposerDrafts
      .getState()
      .setPastedTexts(chat.id, [
        { id: "paste-1", text: "First source line\nSecond source line" },
      ]);
    await renderWithRouter(<DraftingChatView />);

    await userEvent.click(
      screen.getByRole("button", { name: "Steer active response" }),
    );

    await waitFor(() =>
      expect(client.steer).toHaveBeenCalledWith(
        "chat-1",
        "turn-1",
        expect.any(String),
        "<pasted_text>\nFirst source line\nSecond source line\n</pasted_text>",
        true,
        false,
        [],
      ),
    );
    expect(screen.queryByText("Pasted text")).toBeNull();
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

  it("keeps the composer draft across a Settings round trip", async () => {
    const user = userEvent.setup();
    const { router } = await mountConversationShell();

    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "unsent thought",
    );
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(
      "unsent thought",
    );

    await user.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByTestId("settings")).toBeInTheDocument();
    expect(
      screen.queryByRole("textbox", { name: "Message" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Back to app" }));
    expect(await screen.findByRole("textbox", { name: "Message" })).toHaveValue(
      "unsent thought",
    );
    expect(router.state.location.pathname).toBe("/c/chat-1");
  });

  it("keeps drafts scoped to their conversation and forgets them on send", async () => {
    const user = userEvent.setup();
    const { router } = await mountConversationShell();

    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "only for roadmap",
    );

    await user.click(screen.getByRole("button", { name: "Other work" }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/c/chat-2"),
    );
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("");

    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "only for other",
    );

    await user.click(screen.getByRole("button", { name: "Roadmap" }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/c/chat-1"),
    );
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(
      "only for roadmap",
    );

    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("");

    await user.click(screen.getByRole("button", { name: "Settings" }));
    await screen.findByTestId("settings");
    await user.click(screen.getByRole("button", { name: "Back to app" }));

    expect(await screen.findByRole("textbox", { name: "Message" })).toHaveValue(
      "",
    );
    expect(useComposerDrafts.getState().drafts[chat.id]).toBeFalsy();

    await user.click(screen.getByRole("button", { name: "Other work" }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/c/chat-2"),
    );
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(
      "only for other",
    );

    useComposerDrafts.getState().clearDraft(otherChat.id);
    await user.click(screen.getByRole("button", { name: "Settings" }));
    await screen.findByTestId("settings");
    await user.click(screen.getByRole("button", { name: "Back to app" }));
    expect(await screen.findByRole("textbox", { name: "Message" })).toHaveValue(
      "",
    );
  });
});

function ConversationChatPage() {
  const navigate = useNavigate();
  const { chatId } = useParams({ strict: false }) as { chatId: string };
  const openChat = chatId === otherChat.id ? otherChat : chat;

  return (
    <>
      <button type="button" onClick={() => void navigate({ to: "/settings" })}>
        Settings
      </button>
      <button
        type="button"
        onClick={() =>
          void navigate({ to: "/c/$chatId", params: { chatId: chat.id } })
        }
      >
        Roadmap
      </button>
      <button
        type="button"
        onClick={() =>
          void navigate({
            to: "/c/$chatId",
            params: { chatId: otherChat.id },
          })
        }
      >
        Other work
      </button>
      <ChatView
        client={client}
        chat={openChat}
        hydrated
        nativeHost={false}
        deletingChat={false}
        composerModelMenu={null}
        composerPermissionMenu={null}
        composerImages={noImages()}
        files={noFiles()}
        voiceInputUsed={false}
        onVoiceInputAccepted={vi.fn()}
        attachError={null}
        onDraftChange={(value) =>
          useComposerDrafts.getState().setDraft(openChat.id, value)
        }
        onSelectPrompt={vi.fn()}
        onSend={async () => {
          useComposerDrafts.getState().setDraft(openChat.id, "");
        }}
      />
    </>
  );
}

function SettingsPage() {
  const router = useRouter();
  return (
    <>
      <div data-testid="settings">settings</div>
      <button
        type="button"
        onClick={() => {
          if (router.history.canGoBack()) router.history.back();
        }}
      >
        Back to app
      </button>
    </>
  );
}

async function mountConversationShell() {
  const rootRoute = createRootRoute();
  const chatRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/c/$chatId",
    component: ConversationChatPage,
  });
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings",
    component: SettingsPage,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([chatRoute, settingsRoute]),
    history: createMemoryHistory({ initialEntries: ["/c/chat-1"] }),
  });
  await router.load();
  const result = render(<RouterProvider router={router as never} />);
  return { ...result, router };
}
