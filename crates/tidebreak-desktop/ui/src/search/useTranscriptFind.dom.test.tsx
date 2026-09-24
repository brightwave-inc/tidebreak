// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { MessageSearchHit, MessageSearchPage } from "../generated/wire";
import { messageSearchHit, messageSearchIndexed } from "../stories/fixtures";
import { TranscriptFindBar } from "./TranscriptFindBar";
import {
  findCountLabel,
  findInConversation,
  MAX_FIND_MATCHES,
  type TranscriptFindState,
  useTranscriptFind,
} from "./useTranscriptFind";

afterEach(cleanup);

function hits(count: number, offset = 0): MessageSearchHit[] {
  return Array.from({ length: count }, (_, index) =>
    messageSearchHit(`harbour ${offset + index}`, "harbour", {
      message_id: `m${offset + index}`,
    }),
  );
}

/** A client that pages `total` matches, fifty at a time. */
function pagingClient(total: number) {
  return {
    searchMessages: vi.fn(
      async (query: { cursor?: string }): Promise<MessageSearchPage> => {
        const start = query.cursor ? Number(query.cursor) : 0;
        const end = Math.min(total, start + 50);
        return {
          hits: hits(end - start, start),
          next_cursor: end < total ? String(end) : undefined,
          indexing: messageSearchIndexed,
        };
      },
    ),
  };
}

describe("finding in one conversation", () => {
  it("reads every page of matches for the count, held to the conversation", async () => {
    const client = pagingClient(120);
    const found = await findInConversation(
      client,
      "chat-1",
      "harbour",
      new AbortController().signal,
    );
    expect(found.matches).toHaveLength(120);
    expect(found.capped).toBe(false);
    expect(client.searchMessages).toHaveBeenCalledTimes(3);
    expect(client.searchMessages.mock.calls[0]![0]).toMatchObject({
      sessionId: "chat-1",
      q: "harbour",
    });
  });

  it("stops reading at the cap and says there are more", async () => {
    const found = await findInConversation(
      pagingClient(MAX_FIND_MATCHES + 60),
      "chat-1",
      "harbour",
      new AbortController().signal,
    );
    expect(found.matches).toHaveLength(MAX_FIND_MATCHES);
    expect(found.capped).toBe(true);
  });

  it("shows the newest match first, and steps older and newer, wrapping", async () => {
    const onReveal = vi.fn();
    const { result } = renderHook(() =>
      useTranscriptFind({
        client: pagingClient(3),
        sessionId: "chat-1",
        open: true,
        onReveal,
        delayMs: 0,
      }),
    );
    act(() => result.current.setQuery("harbour"));
    await vi.waitFor(() => expect(result.current.state.status).toBe("ready"));
    expect(findCountLabel(result.current.state)).toBe("1 of 3");
    expect(onReveal).toHaveBeenLastCalledWith(
      expect.objectContaining({ message_id: "m0" }),
      ["harbour"],
    );

    act(() => result.current.older());
    expect(findCountLabel(result.current.state)).toBe("2 of 3");
    expect(onReveal).toHaveBeenLastCalledWith(
      expect.objectContaining({ message_id: "m1" }),
      ["harbour"],
    );
    act(() => result.current.older());
    act(() => result.current.older());
    expect(findCountLabel(result.current.state)).toBe("1 of 3");

    act(() => result.current.newer());
    expect(findCountLabel(result.current.state)).toBe("3 of 3");
    expect(onReveal).toHaveBeenLastCalledWith(
      expect.objectContaining({ message_id: "m2" }),
      ["harbour"],
    );
  });

  it("says when nothing matched, and steps nowhere", async () => {
    const onReveal = vi.fn();
    const { result } = renderHook(() =>
      useTranscriptFind({
        client: pagingClient(0),
        sessionId: "chat-1",
        open: true,
        onReveal,
        delayMs: 0,
      }),
    );
    act(() => result.current.setQuery("nothing"));
    await vi.waitFor(() => expect(result.current.state.status).toBe("ready"));
    expect(findCountLabel(result.current.state)).toBe("No matches");
    act(() => result.current.older());
    expect(onReveal).not.toHaveBeenCalled();
  });
});

describe("the find bar", () => {
  const ready: TranscriptFindState = {
    status: "ready",
    query: "harbour",
    matches: hits(12),
    capped: false,
    position: 2,
    indexing: messageSearchIndexed,
    error: null,
  };

  function mount(state: TranscriptFindState = ready) {
    const props = {
      onQueryChange: vi.fn(),
      onOlder: vi.fn(),
      onNewer: vi.fn(),
      onClose: vi.fn(),
    };
    render(<TranscriptFindBar query="harbour" state={state} {...props} />);
    return props;
  }

  it("counts matches and steps with Enter and Shift+Enter", async () => {
    const props = mount();
    expect(screen.getByText("3 of 12")).toBeInTheDocument();
    const field = screen.getByRole("textbox", { name: "Find in conversation" });
    field.focus();
    await userEvent.keyboard("{Enter}");
    expect(props.onOlder).toHaveBeenCalledTimes(1);
    await userEvent.keyboard("{Shift>}{Enter}{/Shift}");
    expect(props.onNewer).toHaveBeenCalledTimes(1);
    await userEvent.keyboard("{Escape}");
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it("leaves Enter and Escape to an IME composition in progress", () => {
    const props = mount();
    const field = screen.getByRole("textbox", { name: "Find in conversation" });
    fireEvent.keyDown(field, { key: "Enter", isComposing: true });
    fireEvent.keyDown(field, { key: "Enter", keyCode: 229 });
    fireEvent.keyDown(field, { key: "Escape", isComposing: true });
    expect(props.onOlder).not.toHaveBeenCalled();
    expect(props.onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(field, { key: "Enter" });
    expect(props.onOlder).toHaveBeenCalledTimes(1);
  });

  it("names its controls, and disables the steps with nothing to step to", () => {
    mount({ ...ready, matches: [], position: -1 });
    expect(screen.getByText("No matches")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Older match" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Newer match" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Close find" })).toBeEnabled();
    expect(screen.getByRole("search")).toBeInTheDocument();
  });

  it("says when the conversation is still being indexed", () => {
    mount({
      ...ready,
      indexing: {
        complete: false,
        pending_conversations: 1,
        failed_conversations: 0,
      },
    });
    expect(screen.getByText(/still being indexed/)).toBeInTheDocument();
  });
});
