// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { useOpenConversation } from "./OpenConversation";
import { useChatListStore } from "./ChatListStore";

describe("useOpenConversation", () => {
  beforeEach(() => {
    useChatListStore.setState({ deletingChatId: null });
  });

  it("accepts a response for the conversation it started in", () => {
    const { result } = renderHook(() => useOpenConversation("chat-1"));
    expect(result.current("chat-1")).toBe(true);
    expect(result.current("chat-2")).toBe(false);
  });

  it("rejects a response for a conversation being deleted", () => {
    const { result } = renderHook(() => useOpenConversation("chat-1"));
    // The ref is written while rendering, so the store change has to land as a
    // render for the predicate to see it.
    act(() => useChatListStore.setState({ deletingChatId: "chat-1" }));
    // Deletion is the case keying misses: the doomed chat stays mounted with a
    // matching id for the whole round trip.
    expect(result.current("chat-1")).toBe(false);
  });

  it("reads current truth rather than the value captured at render", () => {
    const { result, rerender } = renderHook(
      ({ chatId }) => useOpenConversation(chatId),
      { initialProps: { chatId: "chat-1" } },
    );
    const captured = result.current;
    rerender({ chatId: "chat-2" });
    // A handler captures the predicate before an await and asks afterwards.
    expect(captured("chat-1")).toBe(false);
    expect(captured("chat-2")).toBe(true);
  });

  it("reports nothing open once the hook is gone", () => {
    const { result, unmount } = renderHook(() => useOpenConversation("chat-1"));
    const captured = result.current;
    expect(captured("chat-1")).toBe(true);
    unmount();
    // Its inputs are only written while rendering, so without an unmount
    // cleanup this keeps calling a conversation that no longer exists open —
    // and disagrees with the self-resets its callers run on the same event.
    expect(captured("chat-1")).toBe(false);
  });
});
