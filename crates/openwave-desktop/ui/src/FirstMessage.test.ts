import { describe, expect, it } from "vitest";

import { createFirstMessageStore } from "./FirstMessage";

describe("first message handover", () => {
  it("hands the text to the chat it was written for", () => {
    const store = createFirstMessageStore();
    store.getState().hold("chat-1", "summarise the filing");

    expect(store.getState().take("chat-1")).toBe("summarise the filing");
  });

  it("gives it up only once", () => {
    // The chat route's effect can run again on a re-render, and a second read
    // would post the message twice.
    const store = createFirstMessageStore();
    store.getState().hold("chat-1", "hello");

    expect(store.getState().take("chat-1")).toBe("hello");
    expect(store.getState().take("chat-1")).toBeNull();
  });

  it("withholds it from a conversation it was not meant for", () => {
    const store = createFirstMessageStore();
    store.getState().hold("chat-1", "hello");

    expect(store.getState().take("chat-2")).toBeNull();
    // Still waiting for the chat it belongs to.
    expect(store.getState().take("chat-1")).toBe("hello");
  });

  it("has nothing to give when nothing was written", () => {
    const store = createFirstMessageStore();

    expect(store.getState().take("chat-1")).toBeNull();
  });
});
