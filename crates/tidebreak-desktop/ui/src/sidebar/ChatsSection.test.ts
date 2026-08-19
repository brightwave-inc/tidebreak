import { describe, expect, it } from "vitest";

import type { Chat } from "@/api";
import { listedChats, matchesChatSearch } from "./ChatsSection";

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
    // The list labels these "New work", so that is what searching for them has
    // to find — matching nothing would make them unreachable.
    expect(matchesChatSearch(chat(null), "new")).toBe(true);
    expect(matchesChatSearch(chat("   "), "new work")).toBe(true);
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
