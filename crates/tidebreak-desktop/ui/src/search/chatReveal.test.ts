import { describe, expect, it, vi } from "vitest";

import type { ApiClient, ChatTranscript } from "../api";
import {
  type ChatSessionState,
  initialChatSessionState,
} from "../ChatSessionReducer";
import {
  presentChatTranscript,
  TRANSCRIPT_PAGE_TURNS,
} from "../ChatTranscriptPresentation";
import { loadChatMessage, loadEarlierHistory } from "./chatReveal";

/** Turns `from` through `to` of a long conversation: a question and its answer each. */
function transcriptOf(
  from: number,
  to: number,
  cursors: { earlier?: number | null; later?: number },
): ChatTranscript {
  const messages: ChatTranscript["messages"] = [];
  for (let turn = from; turn <= to; turn += 1) {
    const at = new Date(Date.UTC(2026, 8, 1, 0, turn)).toISOString();
    messages.push(
      {
        id: `q${turn}`,
        turn_id: `t${turn}`,
        role: "user",
        content: `question ${turn}`,
        created_at: at,
        citations: [],
      },
      {
        id: `a${turn}`,
        turn_id: `t${turn}`,
        role: "assistant",
        content: `answer ${turn}`,
        created_at: at,
        citations: [],
      },
    );
  }
  const earlier = cursors.earlier ?? null;
  return {
    messages,
    tool_activity: [],
    terminal_turns: [],
    last_event_seq: 900,
    has_more: earlier !== null,
    earlier_cursor: earlier,
    later_cursor: cursors.later,
    answer_versions: [],
  } as ChatTranscript;
}

/**
 * A session holding the newest page of a 400-turn conversation: turns 361
 * through 400. The page before it starts at message sequence 721.
 */
function heldNewestPage(): ChatSessionState {
  const page = presentChatTranscript(
    transcriptOf(361, 400, { earlier: 721 }),
  );
  return {
    ...initialChatSessionState(),
    messages: page.messages,
    hydratedMessageIds: page.messageIds,
    earlierCursor: page.earlierCursor,
  };
}

function harness(answer: ChatTranscript) {
  let state = heldNewestPage();
  const listChatMessages = vi.fn<ApiClient["listChatMessages"]>(
    async () => answer,
  );
  return {
    client: { listChatMessages },
    listChatMessages,
    session: () => state,
    update: (change: (current: ChatSessionState) => ChatSessionState) => {
      state = change(state);
    },
    get state() {
      return state;
    },
  };
}

describe("opening a found message of a long chat", () => {
  it("reads only the page that holds a message on an unloaded page, and shows it apart", async () => {
    // Turn 12 of 400: its page ends a few turns later and leaves a gap
    // before the newest page the transcript holds.
    const env = harness(transcriptOf(1, 15, { earlier: null, later: 31 }));
    const loaded = await loadChatMessage({
      client: env.client,
      chatId: "chat-1",
      messageId: "a12",
      session: env.session,
      update: env.update,
      view: null,
    });

    expect(env.listChatMessages).toHaveBeenCalledTimes(1);
    expect(env.listChatMessages).toHaveBeenCalledWith("chat-1", {
      around: "a12",
      limit: TRANSCRIPT_PAGE_TURNS,
    });
    expect(loaded?.kind).toBe("history");
    if (loaded?.kind !== "history") return;
    expect(loaded.view.messages.some((message) => message.id === "a12")).toBe(
      true,
    );
    // The newest pages stay as they were: nothing between was read.
    expect(env.state.messages[0]?.id).toBe("q361");
    expect(env.state.earlierCursor).toBe(721);
  });

  it("joins the page to the held ones when the two meet", async () => {
    // Turn 350's page runs up to where the held page starts.
    const env = harness(transcriptOf(320, 360, { earlier: 639, later: 721 }));
    const loaded = await loadChatMessage({
      client: env.client,
      chatId: "chat-1",
      messageId: "q350",
      session: env.session,
      update: env.update,
      view: null,
    });

    expect(loaded).toEqual({ kind: "held" });
    expect(env.state.messages[0]?.id).toBe("q320");
    expect(env.state.messages.some((message) => message.id === "q350")).toBe(
      true,
    );
    expect(env.state.messages.at(-1)?.id).toBe("a400");
    expect(env.state.earlierCursor).toBe(639);
  });

  it("reads nothing for a message the transcript already holds", async () => {
    const env = harness(transcriptOf(1, 1, {}));
    await expect(
      loadChatMessage({
        client: env.client,
        chatId: "chat-1",
        messageId: "a399",
        session: env.session,
        update: env.update,
        view: null,
      }),
    ).resolves.toEqual({ kind: "held" });
    expect(env.listChatMessages).not.toHaveBeenCalled();
  });

  it("says so when the conversation no longer has the message", async () => {
    // The server reads the newest page for a message it does not hold.
    const env = harness(transcriptOf(361, 400, { earlier: 721 }));
    await expect(
      loadChatMessage({
        client: env.client,
        chatId: "chat-1",
        messageId: "gone",
        session: env.session,
        update: env.update,
        view: null,
      }),
    ).resolves.toEqual({ kind: "missing" });
  });

  it("pages back through a stretch of history from where it starts", async () => {
    const env = harness(transcriptOf(1, 5, { earlier: null, later: 11 }));
    const view = {
      messages: presentChatTranscript(transcriptOf(6, 10, { earlier: 11 }))
        .messages,
      messageIds: new Set<string>(),
      answerVersions: {},
      earlierCursor: 11,
    };
    const next = await loadEarlierHistory(env.client, "chat-1", view);
    expect(env.listChatMessages).toHaveBeenCalledWith("chat-1", {
      before: 11,
      limit: TRANSCRIPT_PAGE_TURNS,
    });
    expect(next.messages[0]?.id).toBe("q1");
    expect(next.messages.at(-1)?.id).toBe("a10");
    expect(next.earlierCursor).toBeNull();
  });
});
