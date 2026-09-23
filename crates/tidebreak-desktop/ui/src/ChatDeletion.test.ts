import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import { HttpError } from "./api/client/http";
import {
  chatDeletionErrorMessage,
  deletionDescription,
  detachChatFolders,
  inspectLiveChatWork,
  liveChatWorkIsBlocking,
  nextChatAfterDelete,
  purgeDeletedChatHostAuthority,
  stopLiveChatWork,
  tryDeleteChat,
  waitForChatQuiescent,
} from "./ChatDeletion";
import {
  disconnectFolder,
  hasNativeHost,
  purgeDeletedConversationSubject,
} from "./host";

vi.mock("./host", () => ({
  disconnectFolder: vi.fn(async () => true),
  hasNativeHost: vi.fn(() => true),
  purgeDeletedConversationSubject: vi.fn(async () => true),
}));

function chat(id: string, projectId: string | null): Chat {
  return {
    id,
    project_id: projectId,
    title: null,
    model: null,
    reasoning_effort: null,
    permission_mode: null,
    network_policy: { mode: "off" },
    attachment_revision: 0,
    memory_incognito: false,
    last_activity_at: "2026-07-21T12:00:00Z",
    pinned_at: null,
    archived_at: null,
    running: false,
    unread: false,
    turn_count: 1,
    root_attachments: [],
    created_at: "2026-07-21T12:00:00Z",
  };
}

function withFolders(target: Chat, rootIds: string[]): Chat {
  return {
    ...target,
    root_attachments: rootIds.map((root_id) => ({
      root_id,
      origin: "conversation" as const,
    })),
  };
}

describe("where deleting the open chat lands", () => {
  it("opens the top of the list, skipping a chat nothing happened in", () => {
    const empty = { ...chat("empty", null), title: null, turn_count: 0 };
    const worked = chat("worked", null);
    expect(nextChatAfterDelete([empty, worked])?.id).toBe("worked");
  });

  it("goes home rather than creating a chat when nothing is left", () => {
    const empty = { ...chat("empty", null), title: null, turn_count: 0 };
    expect(nextChatAfterDelete([])).toBeNull();
    expect(nextChatAfterDelete([empty])).toBeNull();
  });
});

describe("connected folders on delete", () => {
  beforeEach(() => {
    vi.mocked(disconnectFolder).mockClear();
    vi.mocked(hasNativeHost).mockReturnValue(true);
  });

  // The server refuses to delete a chat that still holds a root, so a folder
  // left attached here is the 409 the reader was shown instead of a delete.
  it("disconnects every folder the chat holds", async () => {
    const target = withFolders(chat("chat-1", null), ["root-a", "root-b"]);
    await detachChatFolders(target);
    expect(
      vi.mocked(disconnectFolder).mock.calls.map((call) => call[1]),
    ).toEqual(["root-a", "root-b"]);
  });

  it("has nothing to disconnect without a native host", async () => {
    vi.mocked(hasNativeHost).mockReturnValue(false);
    await detachChatFolders(withFolders(chat("chat-1", null), ["root-a"]));
    expect(disconnectFolder).not.toHaveBeenCalled();
  });

  it("says what confirming also disconnects", () => {
    expect(deletionDescription({ folders: 0, outputs: 0 })).toBe(
      "This cannot be undone. Archive keeps everything and takes it out of your list.",
    );
    expect(deletionDescription({ folders: 1, outputs: 0 })).toBe(
      "Disconnects 1 connected folder. This cannot be undone. Archive keeps everything and takes it out of your list.",
    );
    expect(deletionDescription({ folders: 2, outputs: 0 })).toContain(
      "Disconnects 2 connected folders.",
    );
  });

  it("says how many outputs go with the conversation", () => {
    expect(deletionDescription({ folders: 0, outputs: 3 })).toBe(
      "Deletes 3 outputs. Export them first if you need them. This cannot be undone. Archive keeps everything and takes it out of your list.",
    );
    expect(deletionDescription({ folders: 0, outputs: 1 })).toContain(
      "Deletes 1 output. Export it first if you need it.",
    );
    // An unread count still warns rather than going quiet.
    expect(deletionDescription({ folders: 0, outputs: null })).toContain(
      "Deletes any outputs it made.",
    );
  });

  it("does not offer the archive to a conversation already in it", () => {
    expect(
      deletionDescription({ folders: 0, outputs: 2, offerArchive: false }),
    ).toBe(
      "Deletes 2 outputs. Export them first if you need them. This cannot be undone.",
    );
  });

  it("says stop and delete when work is still running", () => {
    expect(
      deletionDescription({ folders: 0, outputs: 0, stopping: true }),
    ).toContain("Stops the running response and background agents.");
    expect(
      deletionDescription({ folders: 1, outputs: 2, stopping: true }),
    ).toBe(
      "Stops the running response and background agents. Disconnects 1 connected folder. Deletes 2 outputs. Export them first if you need them. This cannot be undone. Archive keeps everything and takes it out of your list.",
    );
  });
});

describe("active work on delete", () => {
  it("treats a running turn or background agent as blocking", () => {
    expect(
      liveChatWorkIsBlocking({
        activeTurnId: "turn-1",
        backgroundRunIds: [],
      }),
    ).toBe(true);
    expect(
      liveChatWorkIsBlocking({
        activeTurnId: null,
        backgroundRunIds: ["run-1"],
      }),
    ).toBe(true);
    expect(
      liveChatWorkIsBlocking({ activeTurnId: null, backgroundRunIds: [] }),
    ).toBe(false);
  });

  it("reads the open chat's turn and live background runs", async () => {
    const work = await inspectLiveChatWork({
      chatId: "chat-1",
      openChatId: "chat-1",
      session: { busy: true, activeTurnId: "turn-1" },
      listAgentRuns: async () => [
        {
          id: "run-1",
          parent_id: null,
          tier: "background",
          execution_location: "in_process",
          code_execution_provider: "docker",
          status: "running",
          model_steps: 0,
          usage: {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
          },
          task: null,
          started_at: null,
          finished_at: null,
          last_error_code: null,
          activity: null,
          submitted_outputs: [],
          terminal_text: null,
          created_at: "2026-07-21T12:00:00Z",
          updated_at: "2026-07-21T12:00:00Z",
          spawn_call_id: null,
        },
        {
          id: "run-done",
          parent_id: null,
          tier: "background",
          execution_location: "in_process",
          code_execution_provider: "docker",
          status: "completed",
          model_steps: 0,
          usage: {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
          },
          task: null,
          started_at: null,
          finished_at: null,
          last_error_code: null,
          activity: null,
          submitted_outputs: [],
          terminal_text: null,
          created_at: "2026-07-21T12:00:00Z",
          updated_at: "2026-07-21T12:00:00Z",
          spawn_call_id: null,
        },
      ],
    });
    expect(work).toEqual({
      activeTurnId: "turn-1",
      backgroundRunIds: ["run-1"],
    });
  });

  it("does not treat another chat's session as this chat's turn", async () => {
    const work = await inspectLiveChatWork({
      chatId: "chat-1",
      openChatId: "chat-2",
      session: { busy: true, activeTurnId: "turn-other" },
      listAgentRuns: async () => [],
    });
    expect(work).toEqual({ activeTurnId: null, backgroundRunIds: [] });
  });

  it("cancels the turn and background agents before any detach", async () => {
    const cancelTurn = vi.fn(async () => undefined);
    const cancelAgentRun = vi.fn(async () => undefined);
    await stopLiveChatWork({
      chatId: "chat-1",
      work: { activeTurnId: "turn-1", backgroundRunIds: ["run-1"] },
      cancelTurn,
      cancelAgentRun,
    });
    expect(cancelTurn).toHaveBeenCalledWith("chat-1", "turn-1");
    expect(cancelAgentRun).toHaveBeenCalledWith("chat-1", "run-1");
  });

  it("waits until cancel has drained live work", async () => {
    const inspect = vi
      .fn()
      .mockResolvedValueOnce({
        activeTurnId: "turn-1",
        backgroundRunIds: [],
      })
      .mockResolvedValueOnce({ activeTurnId: null, backgroundRunIds: [] });
    const wait = vi.fn(async () => undefined);
    await expect(
      waitForChatQuiescent({ inspect, wait, attempts: 5, intervalMs: 1 }),
    ).resolves.toBe(true);
    expect(inspect).toHaveBeenCalledTimes(2);
  });

  it("asks the server to delete before detaching folders", async () => {
    const deleteChat = vi.fn(async () => undefined);
    await expect(tryDeleteChat(deleteChat)).resolves.toBe("deleted");
    expect(deleteChat).toHaveBeenCalledTimes(1);
  });

  it("reports a running conversation without detaching folders", async () => {
    const deleteChat = vi.fn(async () => {
      throw new HttpError(409, "still working", "chat_active");
    });
    await expect(tryDeleteChat(deleteChat)).resolves.toBe("chat_active");
  });

  it("reports attached folders only after the server says they are the blocker", async () => {
    const deleteChat = vi.fn(async () => {
      throw new HttpError(409, "folders attached", "chat_roots_attached");
    });
    await expect(tryDeleteChat(deleteChat)).resolves.toBe(
      "chat_roots_attached",
    );
  });

  it("explains an active-work refusal without the raw error", () => {
    expect(
      chatDeletionErrorMessage(
        new HttpError(
          409,
          "409: finish or cancel the active work before deleting this conversation",
          "chat_active",
        ),
      ),
    ).toBe(
      "Stop the active work before deleting this conversation, or choose Stop and delete.",
    );
  });
});

describe("host authority after delete", () => {
  beforeEach(() => {
    vi.mocked(purgeDeletedConversationSubject).mockClear();
    vi.mocked(hasNativeHost).mockReturnValue(true);
  });

  it("purges the deleted conversation subject on the host broker", async () => {
    await purgeDeletedChatHostAuthority("chat-1");
    expect(purgeDeletedConversationSubject).toHaveBeenCalledWith("chat-1");
  });

  it("skips host purge without a native host", async () => {
    vi.mocked(hasNativeHost).mockReturnValue(false);
    await purgeDeletedChatHostAuthority("chat-1");
    expect(purgeDeletedConversationSubject).not.toHaveBeenCalled();
  });
});
