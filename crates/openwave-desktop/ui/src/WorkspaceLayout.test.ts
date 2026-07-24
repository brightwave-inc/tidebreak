import { describe, expect, it } from "vitest";
import {
  DEFAULT_FRACTION,
  EMPTY_LAYOUTS,
  MAX_FRACTION,
  MAX_REMEMBERED_CHATS,
  MIN_FRACTION,
  clampFraction,
  layoutForChat,
  normalizeLayouts,
  rememberChatLayout,
  resolveSlots,
} from "./WorkspaceLayout";

const sources = { surface: { kind: "documents" as const }, expanded: false };

describe("clampFraction", () => {
  it("holds the panel between a quarter and three quarters", () => {
    expect(clampFraction(0.05)).toBe(MIN_FRACTION);
    expect(clampFraction(0.9)).toBe(MAX_FRACTION);
    expect(clampFraction(0.5)).toBe(0.5);
  });

  it("falls back to the default for a value that is not a number", () => {
    for (const value of [Number.NaN, Infinity, "0.5", null, undefined]) {
      expect(clampFraction(value)).toBe(DEFAULT_FRACTION);
    }
  });
});

describe("resolveSlots", () => {
  const wide = { expanded: false, fraction: 0.4, narrow: false };

  it("shows the transcript alone when nothing is open beside it", () => {
    expect(resolveSlots({ ...wide, surface: { kind: "chat" } })).toMatchObject({
      showTranscript: true,
      showPanel: false,
    });
  });

  it("shows both when a surface is open on a wide window", () => {
    expect(
      resolveSlots({ ...wide, surface: { kind: "documents" } }),
    ).toMatchObject({ showTranscript: true, showPanel: true, fraction: 0.4 });
  });

  it("shows one at a time when expanded or narrow", () => {
    expect(
      resolveSlots({ ...wide, surface: { kind: "folders" }, expanded: true }),
    ).toMatchObject({ showTranscript: false, showPanel: true });
    expect(
      resolveSlots({ ...wide, surface: { kind: "folders" }, narrow: true }),
    ).toMatchObject({ showTranscript: false, showPanel: true });
  });

  it("keeps the transcript on a narrow window when nothing is open", () => {
    expect(
      resolveSlots({ ...wide, surface: { kind: "chat" }, narrow: true }),
    ).toMatchObject({ showTranscript: true, showPanel: false });
  });
});

describe("rememberChatLayout", () => {
  it("records a chat with a surface open", () => {
    const layouts = rememberChatLayout(EMPTY_LAYOUTS, "chat-1", sources);
    expect(layoutForChat(layouts, "chat-1")).toEqual(sources);
  });

  it("forgets a chat that is back on the transcript", () => {
    const opened = rememberChatLayout(EMPTY_LAYOUTS, "chat-1", sources);
    const closed = rememberChatLayout(opened, "chat-1", {
      surface: { kind: "chat" },
      expanded: false,
    });
    expect(layoutForChat(closed, "chat-1")).toBeNull();
    expect(Object.keys(closed.chats)).toHaveLength(0);
  });

  it("drops the least recently touched chat past the cap", () => {
    let layouts = EMPTY_LAYOUTS;
    for (let i = 0; i < MAX_REMEMBERED_CHATS + 10; i += 1) {
      layouts = rememberChatLayout(layouts, `chat-${i}`, sources);
    }
    expect(Object.keys(layouts.chats)).toHaveLength(MAX_REMEMBERED_CHATS);
    expect(layoutForChat(layouts, "chat-0")).toBeNull();
    expect(layoutForChat(layouts, `chat-${MAX_REMEMBERED_CHATS + 9}`)).toEqual(
      sources,
    );
  });

  it("moves a revisited chat back to the front", () => {
    let layouts = rememberChatLayout(EMPTY_LAYOUTS, "chat-1", sources);
    layouts = rememberChatLayout(layouts, "chat-2", sources);
    layouts = rememberChatLayout(layouts, "chat-1", sources);
    expect(Object.keys(layouts.chats)).toEqual(["chat-1", "chat-2"]);
  });

  it("leaves the width preference alone", () => {
    const layouts = rememberChatLayout(
      { fraction: 0.6, chats: {} },
      "chat-1",
      sources,
    );
    expect(layouts.fraction).toBe(0.6);
  });
});

describe("normalizeLayouts", () => {
  it("restores a stored layout", () => {
    const stored = { fraction: 0.6, chats: { "chat-1": sources } };
    expect(normalizeLayouts(JSON.parse(JSON.stringify(stored)))).toEqual(
      stored,
    );
  });

  it("drops a chat whose surface does not open beside the transcript", () => {
    const layouts = normalizeLayouts({
      fraction: 0.5,
      chats: {
        "chat-1": { surface: { kind: "settings" }, expanded: false },
        "chat-2": { surface: { kind: "chat" }, expanded: false },
        "chat-3": { surface: { kind: "nonsense" }, expanded: false },
      },
    });
    expect(layouts.chats).toEqual({});
  });

  it("normalizes the surface it restores", () => {
    const layouts = normalizeLayouts({
      chats: {
        "chat-1": {
          surface: { kind: "deliverables", itemId: "notes.md", anchor: "c-1" },
          expanded: "yes",
        },
      },
    });
    expect(layouts.chats["chat-1"]).toEqual({
      surface: { kind: "deliverables", itemId: "notes.md" },
      expanded: false,
    });
  });

  it("falls back for anything unrecognisable", () => {
    for (const value of [null, undefined, "{}", [], 4]) {
      expect(normalizeLayouts(value)).toEqual(EMPTY_LAYOUTS);
    }
    expect(normalizeLayouts({ fraction: "wide" })).toEqual(EMPTY_LAYOUTS);
  });
});
