import { describe, expect, it } from "vitest";
import type { Chat } from "./api";
import { existingChatAfterDeletion, prependReplacementChat } from "./ChatDeletion";

function chat(id: string, projectId: string | null): Chat {
  return {
    id,
    project_id: projectId,
    title: null,
    model: null,
    attachment_revision: 0,
    root_attachments: [],
    created_at: "2026-07-21T12:00:00Z",
  };
}

describe("chat deletion replacement", () => {
  it("preserves loose scope without hiding project conversations", () => {
    const projectChat = chat("project-chat", "project-1");
    const refreshed = [projectChat];

    expect(existingChatAfterDeletion(refreshed, null)).toBeUndefined();

    const replacement = chat("loose-replacement", null);
    expect(prependReplacementChat(refreshed, replacement)).toEqual([
      replacement,
      projectChat,
    ]);
  });

  it("leaves an emptied project by selecting another existing chat", () => {
    const loose = chat("loose-chat", null);
    expect(existingChatAfterDeletion([loose], "project-1")).toBe(loose);
  });
});
