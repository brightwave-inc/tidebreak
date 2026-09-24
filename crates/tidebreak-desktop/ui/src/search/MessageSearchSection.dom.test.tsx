// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CommandPaletteList } from "../CommandPaletteList";
import type { MessageSearchHit } from "../generated/wire";
import {
  archivedMessageSearchHit,
  messageSearchHits,
  messageSearchIndexed,
  messageSearchNow,
} from "../stories/fixtures";
import type { MessageSearchState } from "./useMessageSearch";

afterEach(cleanup);

function state(overrides: Partial<MessageSearchState>): MessageSearchState {
  return {
    status: "ready",
    query: "harbour",
    hits: [],
    indexing: messageSearchIndexed,
    error: null,
    ...overrides,
  };
}

function mount(messages: MessageSearchState) {
  const onSelectMessage = vi.fn<(hit: MessageSearchHit) => void>();
  render(
    <CommandPaletteList
      groups={[]}
      query="harbour"
      onQueryChange={vi.fn()}
      onSelect={vi.fn()}
      command
      messages={messages}
      onSelectMessage={onSelectMessage}
      now={messageSearchNow}
    />,
  );
  return { onSelectMessage };
}

describe("the palette's Messages section", () => {
  it("marks exactly the ranges the server matched, and hands back the hit", () => {
    const { onSelectMessage } = mount(state({ hits: messageSearchHits }));
    expect(screen.getByText("Messages")).toBeInTheDocument();
    const options = screen.getAllByRole("option");
    expect(options).toHaveLength(messageSearchHits.length);

    const first = options[0]!;
    const marks = within(first)
      .getAllByText((_, node) => node?.tagName === "MARK")
      .map((mark) => mark.textContent);
    expect(marks).toEqual(["harbour"]);
    expect(first).toHaveTextContent("Harbour pricing notes");
    expect(first).toHaveTextContent("Chat");

    const code = options[2]!;
    expect(code).toHaveTextContent("fix-harbour-rounding");
    expect(code).toHaveTextContent("Code");

    first.click();
    expect(onSelectMessage).toHaveBeenCalledWith(messageSearchHits[0]);
  });

  it("names an archived conversation's hit as archived", () => {
    mount(state({ hits: [archivedMessageSearchHit] }));
    const option = screen.getByRole("option");
    expect(option).toHaveTextContent("Archived");
    expect(option.getAttribute("aria-label")).toContain("archived");
  });

  it("says a search is running before any hit is in", () => {
    mount(state({ status: "loading", hits: [] }));
    expect(screen.getByText("Searching messages…")).toBeInTheDocument();
    // The list's own "nothing matches" waits for the search to answer.
    expect(screen.queryByText(/Nothing here/)).not.toBeInTheDocument();
  });

  it("says nothing matched, in the reader's words", () => {
    mount(state({ hits: [] }));
    expect(
      screen.getByText("No messages match “harbour”."),
    ).toBeInTheDocument();
  });

  it("says the index is still catching up, so a miss is not final", () => {
    mount(
      state({
        hits: [],
        indexing: {
          complete: false,
          pending_conversations: 12,
          failed_conversations: 0,
        },
      }),
    );
    expect(screen.getByRole("listbox")).toHaveTextContent(
      /Still indexing 12 conversations/,
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "No messages match. Still indexing 12 conversations.",
    );
  });

  it("notes, quietly, conversations the index could not add", () => {
    mount(
      state({
        hits: messageSearchHits.slice(0, 1),
        indexing: {
          complete: false,
          pending_conversations: 0,
          failed_conversations: 1,
        },
      }),
    );
    expect(
      screen.getByText(/1 conversation could not be indexed/),
    ).toBeInTheDocument();
  });

  it("says a search failed without taking the rest of the palette down", () => {
    mount(state({ status: "error", error: "Could not search messages." }));
    expect(screen.getByRole("listbox")).toHaveTextContent(
      "Could not search messages.",
    );
  });

  it("announces the search from outside the list, which holds only options", () => {
    mount(state({ hits: messageSearchHits }));
    const status = screen.getByRole("status");
    expect(status).toHaveTextContent("3 messages match");
    expect(screen.getByRole("listbox")).not.toContainElement(status);
    // Inside, the list holds only options and the groups around them.
    for (const node of screen.getByRole("listbox").querySelectorAll("[role]")) {
      expect(["option", "group", "presentation", "listbox"]).toContain(
        node.getAttribute("role"),
      );
    }
  });
});
