import { describe, expect, it } from "vitest";

import type { Chat } from "@/api";
import { matchesChatSearch } from "./ChatsSection";

function chat(title: string | null): Chat {
  return { id: "chat-1", title } as unknown as Chat;
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
    // The list labels these "New chat", so that is what searching for them has
    // to find — matching nothing would make them unreachable.
    expect(matchesChatSearch(chat(null), "new")).toBe(true);
    expect(matchesChatSearch(chat("   "), "new chat")).toBe(true);
    expect(matchesChatSearch(chat(null), "roadmap")).toBe(false);
  });

  it("ignores surrounding whitespace in the query", () => {
    expect(matchesChatSearch(chat("Roadmap"), "  road  ")).toBe(true);
  });
});
