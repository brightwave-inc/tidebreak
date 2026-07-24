// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import { ChatView, type ChatViewProps } from "./ChatView";
import { useChatSessionStore } from "./ChatSessionStore";
import type { ConversationRequests } from "./ConversationRequests";
import {
  idleConversationRequests,
  withConversationRequests,
} from "./test/conversationRequests";

const chat = { id: "chat-1", title: "Roadmap", project_id: null } as unknown as Chat;

function renderChatView(
  overrides: Partial<ChatViewProps> = {},
  requests: ConversationRequests = idleConversationRequests(),
) {
  const props: ChatViewProps = {
    chat,
    hydrated: true,
    nativeHost: false,
    deletingChat: false,
    draft: "",
    composerModelMenu: null,
    attachingSource: false,
    attachedSourceName: null,
    sourceAttachmentError: null,
    onDraftChange: vi.fn(),
    onAddSource: vi.fn(async () => {}),
    onDismissAttachedSource: vi.fn(),
    onSelectPrompt: vi.fn(),
    onSend: vi.fn(async () => {}),
    ...overrides,
  };
  render(withConversationRequests(<ChatView {...props} />, requests));
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
