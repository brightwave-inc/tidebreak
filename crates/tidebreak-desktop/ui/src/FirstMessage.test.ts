import { describe, expect, it } from "vitest";

import { createFirstMessageStore } from "./FirstMessage";

describe("first message handover", () => {
  const pending = {
    text: "summarise the filing",
    images: [],
    files: [],
    pastedTexts: [],
    skills: [],
    voiceInputUsed: false,
  };

  it("hands the message and attachments to the chat they were written for", () => {
    const store = createFirstMessageStore();
    store.getState().hold("chat-1", pending);

    expect(store.getState().take("chat-1")).toEqual(pending);
  });

  it("gives it up only once", () => {
    // The chat route's effect can run again on a re-render, and a second read
    // would post the message twice.
    const store = createFirstMessageStore();
    store.getState().hold("chat-1", pending);

    expect(store.getState().take("chat-1")).toEqual(pending);
    expect(store.getState().take("chat-1")).toBeNull();
  });

  it("withholds it from a conversation it was not meant for", () => {
    const store = createFirstMessageStore();
    store.getState().hold("chat-1", pending);

    expect(store.getState().take("chat-2")).toBeNull();
    // Still waiting for the chat it belongs to.
    expect(store.getState().take("chat-1")).toEqual(pending);
  });

  it("has nothing to give when nothing was written", () => {
    const store = createFirstMessageStore();

    expect(store.getState().take("chat-1")).toBeNull();
  });

  it("hands over pasted text without requiring typed text", () => {
    const store = createFirstMessageStore();
    const pastedOnly = {
      ...pending,
      text: "",
      pastedTexts: [{ id: "paste-1", text: "source material" }],
    };
    store.getState().hold("chat-1", pastedOnly);

    expect(store.getState().take("chat-1")).toEqual(pastedOnly);
  });
});
