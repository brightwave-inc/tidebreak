import { describe, expect, it } from "vitest";
import type { Chat } from "./api";
import { prependReplacementChat } from "./ChatDeletion";

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
