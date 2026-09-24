// @vitest-environment jsdom
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, ChatTranscript } from "../api";
import { initialChatSessionState } from "../ChatSessionReducer";
import { useChatSessionStore } from "../ChatSessionStore";
import { presentChatTranscript } from "../ChatTranscriptPresentation";
import { useTranscriptRevealStore } from "./transcriptReveal";
import { useChatTranscriptSearch } from "./useChatTranscriptSearch";

afterEach(() => {
  cleanup();
  useChatSessionStore.getState().reset();
});

/** Turns `from` through `to` of chat `chat`: a question and its answer each. */
function transcriptOf(
  chat: string,
  from: number,
  to: number,
  cursors: { earlier: number | null; later?: number },
): ChatTranscript {
  const messages: ChatTranscript["messages"] = [];
  for (let turn = from; turn <= to; turn += 1) {
    const at = new Date(Date.UTC(2026, 8, 1, 0, turn)).toISOString();
    messages.push(
      {
        id: `${chat}-q${turn}`,
        turn_id: `${chat}-t${turn}`,
        role: "user",
        content: `question ${turn}`,
        created_at: at,
        citations: [],
      },
      {
        id: `${chat}-a${turn}`,
        turn_id: `${chat}-t${turn}`,
        role: "assistant",
        content: `answer ${turn}`,
        created_at: at,
        citations: [],
      },
    );
  }
  return {
    messages,
    tool_activity: [],
    terminal_turns: [],
    last_event_seq: 900,
    has_more: cursors.earlier !== null,
    earlier_cursor: cursors.earlier,
    later_cursor: cursors.later,
    answer_versions: [],
  } as ChatTranscript;
}

/** Put `transcript` in the session store, the way opening a chat does. */
function open(transcript: ChatTranscript) {
  const page = presentChatTranscript(transcript);
  useChatSessionStore.getState().reset();
  useChatSessionStore.getState().update(() => ({
    ...initialChatSessionState(),
    messages: page.messages,
    hydratedMessageIds: page.messageIds,
    earlierCursor: page.earlierCursor,
  }));
}

function Transcript({
  chatId,
  client,
}: {
  chatId: string;
  client: Pick<ApiClient, "listChatMessages" | "searchMessages">;
}) {
  useChatTranscriptSearch({
    client,
    chatId,
    hydrated: true,
    scrollElement: null,
    disarmFollow: () => {},
    beginProgrammaticScroll: () => {},
    endProgrammaticScroll: () => {},
  });
  return null;
}

describe("a jump into a chat", () => {
  it("writes nothing once another chat is open when its page lands", async () => {
    // A long chat holds its newest page; the match is on an older one.
    open(transcriptOf("long", 361, 400, { earlier: 721 }));
    let answer: (transcript: ChatTranscript) => void = () => {};
    const client = {
      listChatMessages: vi.fn<ApiClient["listChatMessages"]>(
        () =>
          new Promise<ChatTranscript>((resolve) => {
            answer = resolve;
          }),
      ),
      searchMessages: vi.fn<ApiClient["searchMessages"]>(),
    };
    const view = render(
      <Transcript key="long" chatId="long" client={client} />,
    );
    act(() => {
      useTranscriptRevealStore
        .getState()
        .reveal({ kind: "chat", sessionId: "long", messageId: "long-a350" }, [
          "answer",
        ]);
    });
    await waitFor(() =>
      expect(client.listChatMessages).toHaveBeenCalledWith("long", {
        around: "long-a350",
        limit: expect.any(Number),
      }),
    );

    // A short chat opens before the page arrives. It holds its whole
    // history, so a page from the long chat would join above it.
    view.rerender(<Transcript key="short" chatId="short" client={client} />);
    open(transcriptOf("short", 1, 3, { earlier: null }));
    await act(async () => {
      answer(transcriptOf("long", 320, 360, { earlier: 639, later: 721 }));
    });

    expect(
      useChatSessionStore.getState().messages.map((message) => message.id),
    ).toEqual([
      "short-q1",
      "short-a1",
      "short-q2",
      "short-a2",
      "short-q3",
      "short-a3",
    ]);
  });
});
