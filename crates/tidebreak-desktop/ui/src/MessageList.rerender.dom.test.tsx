// @vitest-environment jsdom
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MessageList, type ChatMessage } from "./MessageList";
import { STREAM_STALL_MS } from "./useStreamStalled";

// Every collapsed phase line draws exactly one icon, named for its tool, so
// the icons that render name the phase lines that rendered.
const icons = vi.hoisted(() => ({ rendered: [] as string[] }));
vi.mock("./ToolIcon", () => ({
  ToolIcon: ({ name }: { name: string }) => {
    icons.rendered.push(name);
    return null;
  },
}));

const noop = () => undefined;
const NO_CALLS = new Set<string>();
const NO_ERRORS: Record<string, string> = {};
const scrollRef = { current: null };
const TOOLS = ["search", "read_document", "web_search"];

function list(messages: ChatMessage[]) {
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
      busy
      animateStreaming={false}
      scrollRef={scrollRef}
      onScroll={noop}
      onApproval={noop}
      onFolderAccessDecision={noop}
      onFolderAccessCancel={noop}
    />
  );
}

function turn(
  index: number,
  status: "running" | "completed" = "completed",
): ChatMessage[] {
  return [
    { id: `u${index}`, role: "user", text: `Question ${index}` },
    {
      id: `t${index}`,
      role: "tool",
      callId: `c${index}`,
      name: TOOLS[index]!,
      status,
    },
    {
      id: `a${index}`,
      role: "assistant",
      text: `Answer ${index}`,
      sources: [],
    },
  ];
}

beforeEach(() => {
  icons.rendered = [];
});

afterEach(() => {
  cleanup();
});

describe("re-rendering a long transcript", () => {
  // Every streamed token hands the list a new array. Rebuilding every phase
  // for it re-rendered every rail in the conversation, once per token.
  it("leaves the phases a streamed token did not touch alone", () => {
    const messages = [...turn(0), ...turn(1), ...turn(2)];
    const { rerender } = render(list(messages));
    expect(new Set(icons.rendered)).toEqual(new Set(TOOLS));

    icons.rendered = [];
    const last = messages.at(-1) as Extract<ChatMessage, { role: "assistant" }>;
    rerender(
      list([...messages.slice(0, -1), { ...last, text: `${last.text} more` }]),
    );
    expect(icons.rendered).toEqual([]);
  });

  it("re-renders only the phase whose call changed", () => {
    const running = [...turn(0), ...turn(1), ...turn(2, "running")];
    const { rerender } = render(list(running));
    icons.rendered = [];

    rerender(
      list(
        [...turn(0), ...turn(1), ...turn(2)].map((message, index) =>
          // Everything but the settled call keeps its object, as the reducer does.
          index === 7 ? message : running[index]!,
        ),
      ),
    );
    expect(new Set(icons.rendered)).toEqual(new Set(["web_search"]));
  });
});

describe("the stall check", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  // The stream cursor advances with every event. Read by the whole transcript
  // it re-rendered every row per event; read by one leaf it costs one leaf.
  it("watches the stream cursor itself and shows Working once it goes quiet", () => {
    vi.useFakeTimers();
    let cursor = 1;
    const listeners = new Set<() => void>();
    const streamActivity = {
      subscribe: (onChange: () => void) => {
        listeners.add(onChange);
        return () => listeners.delete(onChange);
      },
      getSnapshot: () => cursor,
    };
    const streaming: ChatMessage[] = [
      { id: "u0", role: "user", text: "Question" },
      { id: "a0", role: "assistant", text: "Partial answer", sources: [] },
    ];
    const { queryByText } = render(
      <MessageList
        messages={streaming}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={NO_CALLS}
        folderAccessErrors={NO_ERRORS}
        decidingApprovalCalls={NO_CALLS}
        approvalErrors={NO_ERRORS}
        busy
        scrollRef={scrollRef}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
        streamActivity={streamActivity}
      />,
    );
    expect(queryByText("Working")).toBeNull();

    act(() => {
      vi.advanceTimersByTime(STREAM_STALL_MS);
    });
    expect(queryByText("Working")).not.toBeNull();

    act(() => {
      cursor += 1;
      for (const onChange of listeners) onChange();
    });
    expect(queryByText("Working")).toBeNull();
  });
});
