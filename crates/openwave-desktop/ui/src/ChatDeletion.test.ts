import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import {
  deletionDescription,
  detachChatFolders,
  prependReplacementChat,
} from "./ChatDeletion";
import { disconnectFolder, hasNativeHost } from "./host";

vi.mock("./host", () => ({
  disconnectFolder: vi.fn(async () => true),
  hasNativeHost: vi.fn(() => true),
}));

function chat(id: string, projectId: string | null): Chat {
  return {
    id,
    project_id: projectId,
    title: null,
    model: null,
    reasoning_effort: null,
    attachment_revision: 0,
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

describe("chat deletion replacement", () => {
  it("puts a new loose replacement ahead of the chats that remain", () => {
    const projectChat = chat("project-chat", "project-1");
    const refreshed = [projectChat];
    const replacement = chat("loose-replacement", null);
    expect(prependReplacementChat(refreshed, replacement)).toEqual([
      replacement,
      projectChat,
    ]);
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
    expect(vi.mocked(disconnectFolder).mock.calls.map((call) => call[1])).toEqual([
      "root-a",
      "root-b",
    ]);
  });

  it("has nothing to disconnect without a native host", async () => {
    vi.mocked(hasNativeHost).mockReturnValue(false);
    await detachChatFolders(withFolders(chat("chat-1", null), ["root-a"]));
    expect(disconnectFolder).not.toHaveBeenCalled();
  });

  it("says what confirming also disconnects", () => {
    expect(deletionDescription(0)).toBe("This cannot be undone.");
    expect(deletionDescription(1)).toBe(
      "Disconnects 1 connected folder first. This cannot be undone.",
    );
    expect(deletionDescription(2)).toBe(
      "Disconnects 2 connected folders first. This cannot be undone.",
    );
  });
});
