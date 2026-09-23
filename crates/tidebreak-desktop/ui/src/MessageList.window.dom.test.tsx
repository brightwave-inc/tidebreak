// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  MessageList,
  TRANSCRIPT_TURNS_SHOWN,
  type ChatMessage,
} from "./MessageList";

const noop = () => undefined;
const NO_CALLS = new Set<string>();
const NO_ERRORS: Record<string, string> = {};

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function turns(count: number, from = 0): ChatMessage[] {
  return Array.from({ length: count }, (_, offset) => {
    const index = from + offset;
    return [
      { id: `u${index}`, role: "user", text: `Question ${index}` },
      {
        id: `a${index}`,
        role: "assistant",
        text: `Answer ${index}`,
        sources: [],
      },
    ] satisfies ChatMessage[];
  }).flat();
}

function list(
  messages: ChatMessage[],
  props: Partial<ComponentProps<typeof MessageList>> = {},
) {
  return (
    <MessageList
      messages={messages}
      folderAccessRequests={[]}
      nativeHost={false}
      nativeBusy={false}
      resolvingFolderCalls={NO_CALLS}
      folderAccessErrors={NO_ERRORS}
      decidingApprovalCalls={NO_CALLS}
      approvalErrors={NO_ERRORS}
      busy={false}
      scrollRef={{ current: null }}
      onScroll={noop}
      onApproval={noop}
      onFolderAccessDecision={noop}
      onFolderAccessCancel={noop}
      {...props}
    />
  );
}

const showEarlier = () =>
  screen.queryByRole("button", { name: "Show earlier messages" });

describe("a long transcript", () => {
  // Rendering every turn of a long conversation on open cost about a second
  // of render and commit. It opens on its newest turns instead.
  it("opens on its newest turns and shows the rest on request", async () => {
    const user = userEvent.setup();
    render(list(turns(TRANSCRIPT_TURNS_SHOWN + 5)));

    expect(screen.queryByText("Question 4")).toBeNull();
    expect(screen.getByText("Question 5")).toBeInTheDocument();
    expect(screen.getByText(`Question ${TRANSCRIPT_TURNS_SHOWN + 4}`));

    await user.click(showEarlier()!);
    expect(screen.getByText("Question 0")).toBeInTheDocument();
    expect(showEarlier()).toBeNull();
  });

  it("keeps new turns on screen as the conversation grows", () => {
    const { rerender } = render(list(turns(TRANSCRIPT_TURNS_SHOWN + 1)));
    expect(screen.queryByText("Question 0")).toBeNull();
    rerender(list(turns(TRANSCRIPT_TURNS_SHOWN + 3)));
    // The window does not slide: the oldest turn shown stays shown.
    expect(screen.getByText("Question 1")).toBeInTheDocument();
    expect(
      screen.getByText(`Question ${TRANSCRIPT_TURNS_SHOWN + 2}`),
    ).toBeInTheDocument();
  });

  it("fetches the page before the held turns once they are all shown", async () => {
    const user = userEvent.setup();
    let finish: (() => void) | undefined;
    let fail: ((error: Error) => void) | undefined;
    const onLoadEarlierMessages = vi.fn(
      () =>
        new Promise<void>((resolve, reject) => {
          finish = resolve;
          fail = reject;
        }),
    );
    render(list(turns(2), { hasEarlierMessages: true, onLoadEarlierMessages }));

    await user.click(showEarlier()!);
    expect(onLoadEarlierMessages).toHaveBeenCalledTimes(1);
    expect(showEarlier()).toBeDisabled();

    await act(async () => fail?.(new Error("offline")));
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Could not load earlier messages",
    );
    expect(showEarlier()).toBeEnabled();

    await user.click(showEarlier()!);
    await act(async () => finish?.());
    expect(onLoadEarlierMessages).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  // The earlier turns land above what the reader was looking at. Without
  // holding the distance from the bottom, that content jumps down by the
  // height of everything revealed.
  it("keeps the reader's place when earlier turns appear above it", async () => {
    const user = userEvent.setup();
    let scroller: HTMLDivElement | null = null;
    render(
      list(turns(TRANSCRIPT_TURNS_SHOWN + 5), {
        scrollRef: (element) => {
          scroller = element;
        },
      }),
    );
    const viewport = scroller! as HTMLDivElement;
    // jsdom lays nothing out; stand in a height per rendered turn.
    Object.defineProperty(viewport, "scrollHeight", {
      configurable: true,
      get: () => viewport.querySelectorAll(".message-turn").length * 100,
    });
    viewport.scrollTop = 150;

    await user.click(showEarlier()!);
    expect(viewport.scrollTop).toBe(150 + 5 * 100);
  });
});
