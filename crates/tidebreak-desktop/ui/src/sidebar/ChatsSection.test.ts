import { describe, expect, it } from "vitest";

import type { Chat } from "@/api";
import type { ChatListGroup } from "@/chatListGroups";
import {
  listedChats,
  matchesChatSearch,
  OLDER_PREVIEW_ROWS,
  visibleGroupRows,
} from "./ChatsSection";

function chat(title: string | null, projectId: string | null = null): Chat {
  return { id: "chat-1", title, project_id: projectId } as unknown as Chat;
}

describe("matchesChatSearch", () => {
  it("keeps everything when nothing has been typed", () => {
    expect(matchesChatSearch(chat("Roadmap"), "")).toBe(true);
    expect(matchesChatSearch(chat(null), "   ")).toBe(true);
  });

  it("matches part of a title, ignoring case", () => {
    expect(matchesChatSearch(chat("Q3 Roadmap"), "roadmap")).toBe(true);
    expect(matchesChatSearch(chat("Q3 Roadmap"), "ROAD")).toBe(true);
    expect(matchesChatSearch(chat("Q3 Roadmap"), "budget")).toBe(false);
  });

  it("searches an untitled chat under the name it is shown by", () => {
    // The list labels these "New conversation", so that is what searching for them has
    // to find — matching nothing would make them unreachable.
    expect(matchesChatSearch(chat(null), "new")).toBe(true);
    expect(matchesChatSearch(chat("   "), "new conversation")).toBe(true);
    expect(matchesChatSearch(chat(null), "roadmap")).toBe(false);
  });

  it("ignores surrounding whitespace in the query", () => {
    expect(matchesChatSearch(chat("Roadmap"), "  road  ")).toBe(true);
  });
});

describe("listedChats", () => {
  it("leaves chats filed under a project to that project's row", () => {
    // The rail shows every conversation exactly once: a chat with a project
    // hangs under the folder above, so listing it here too would duplicate it.
    const loose = chat("Roadmap");
    const filed = chat("Budget", "project-1");
    expect(listedChats([loose, filed], "")).toEqual([loose]);
  });

  it("still applies the filter to the chats it keeps", () => {
    const loose = chat("Roadmap");
    expect(listedChats([loose, chat("Roadmap", "project-1")], "road")).toEqual([
      loose,
    ]);
    expect(listedChats([loose], "budget")).toEqual([]);
  });
});

describe("chats nothing happened in", () => {
  const empty = {
    id: "empty",
    title: null,
    project_id: null,
    turn_count: 0,
    pinned_at: null,
  } as unknown as Chat;

  it("stay out of the list, so an abandoned start leaves no row", () => {
    expect(listedChats([empty], "")).toEqual([]);
  });

  it("show while they are the conversation on screen", () => {
    expect(listedChats([empty], "", "empty")).toEqual([empty]);
  });

  it("show once they are named or pinned", () => {
    const named = { ...empty, title: "Offsite plan" };
    const pinned = { ...empty, pinned_at: "2026-09-20T10:00:00Z" };
    expect(listedChats([named, pinned], "")).toEqual([named, pinned]);
  });
});

describe("the Older preview", () => {
  const older = (count: number): ChatListGroup => ({
    key: "older",
    label: "Older",
    chats: Array.from(
      { length: count },
      (_, index) => ({ id: `older-${index}` }) as unknown as Chat,
    ),
  });

  it("shows every row of a short group", () => {
    const group = older(OLDER_PREVIEW_ROWS);
    expect(visibleGroupRows(group, false)).toEqual({
      rows: group.chats,
      hidden: 0,
    });
  });

  it("stops a long group at the preview and counts the rest", () => {
    const { rows, hidden } = visibleGroupRows(older(30), false);
    expect(rows).toHaveLength(OLDER_PREVIEW_ROWS);
    expect(hidden).toBe(30 - OLDER_PREVIEW_ROWS);
  });

  it("never hides the conversation on screen", () => {
    const { rows, hidden } = visibleGroupRows(older(30), false, "older-27");
    expect(rows.at(-1)?.id).toBe("older-27");
    expect(hidden).toBe(30 - OLDER_PREVIEW_ROWS - 1);
  });

  it("shows everything once asked", () => {
    expect(visibleGroupRows(older(30), true).rows).toHaveLength(30);
  });
});
