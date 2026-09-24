// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { MessageSearchPage } from "../generated/wire";
import {
  messageSearchHit,
  messageSearchIndexed,
} from "../stories/fixtures";
import {
  MESSAGE_SEARCH_DELAY_MS,
  useMessageSearch,
} from "./useMessageSearch";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

function page(word: string): MessageSearchPage {
  return {
    hits: [messageSearchHit(`about ${word}`, word)],
    indexing: messageSearchIndexed,
  };
}

/** A client whose answers the test releases one at a time. */
function deferredClient() {
  const calls: {
    query: string;
    signal: AbortSignal;
    resolve: (page: MessageSearchPage) => void;
  }[] = [];
  const searchMessages = vi.fn(
    (query: { q: string }, signal?: AbortSignal) =>
      new Promise<MessageSearchPage>((resolve) => {
        calls.push({ query: query.q, signal: signal!, resolve });
      }),
  );
  return { client: { searchMessages }, calls, searchMessages };
}

describe("searching messages as the person types", () => {
  it("waits for a pause, then sends only the last query", async () => {
    const { client, calls } = deferredClient();
    const { result, rerender } = renderHook(
      ({ query }) => useMessageSearch(client, query),
      { initialProps: { query: "ha" } },
    );
    for (const query of ["har", "harb", "harbour"]) {
      act(() => void vi.advanceTimersByTime(MESSAGE_SEARCH_DELAY_MS - 20));
      rerender({ query });
    }
    expect(calls).toHaveLength(0);
    expect(result.current.status).toBe("loading");

    act(() => void vi.advanceTimersByTime(MESSAGE_SEARCH_DELAY_MS));
    expect(calls.map((call) => call.query)).toEqual(["harbour"]);

    await act(async () => calls[0]!.resolve(page("harbour")));
    expect(result.current.status).toBe("ready");
    expect(result.current.query).toBe("harbour");
    expect(result.current.hits).toHaveLength(1);
  });

  it("aborts a search still out when the query changes, and never shows its answer", async () => {
    const { client, calls } = deferredClient();
    const { result, rerender } = renderHook(
      ({ query }) => useMessageSearch(client, query),
      { initialProps: { query: "harbour" } },
    );
    act(() => void vi.advanceTimersByTime(MESSAGE_SEARCH_DELAY_MS));
    expect(calls).toHaveLength(1);

    rerender({ query: "lighthouse" });
    expect(calls[0]!.signal.aborted).toBe(true);
    // The stale answer lands after the new query went out; it is dropped.
    await act(async () => calls[0]!.resolve(page("harbour")));
    expect(result.current.status).toBe("loading");

    act(() => void vi.advanceTimersByTime(MESSAGE_SEARCH_DELAY_MS));
    await act(async () => calls[1]!.resolve(page("lighthouse")));
    expect(result.current.query).toBe("lighthouse");
    expect(result.current.hits[0]!.snippet).toBe("about lighthouse");
  });

  it("stays idle for one letter, when disabled, and after the query clears", () => {
    const { client, searchMessages } = deferredClient();
    const { result, rerender } = renderHook(
      ({ query, enabled }) => useMessageSearch(client, query, { enabled }),
      { initialProps: { query: "h", enabled: true } },
    );
    act(() => void vi.advanceTimersByTime(MESSAGE_SEARCH_DELAY_MS * 2));
    expect(result.current.status).toBe("idle");
    rerender({ query: "harbour", enabled: false });
    act(() => void vi.advanceTimersByTime(MESSAGE_SEARCH_DELAY_MS * 2));
    expect(result.current.status).toBe("idle");
    expect(searchMessages).not.toHaveBeenCalled();
  });

  it("says a search failed, in words", async () => {
    const searchMessages = vi.fn(() =>
      Promise.reject(new Error("503: index busy")),
    );
    const { result } = renderHook(() =>
      useMessageSearch({ searchMessages }, "harbour"),
    );
    await act(async () => {
      vi.advanceTimersByTime(MESSAGE_SEARCH_DELAY_MS);
    });
    expect(result.current.status).toBe("error");
    expect(result.current.error).toBeTruthy();
  });
});
