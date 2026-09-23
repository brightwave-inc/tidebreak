// @vitest-environment jsdom
/**
 * How long a long Work-mode chat takes to stream and to open.
 *
 * Run with `pnpm bench`. The numbers come from jsdom, so they are slower than
 * a real webview and only mean something next to each other: compare a run
 * before a change with a run after it, on the same machine.
 */
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { test, vi, type TestContext } from "vitest";

import type { ChatFrame, SequencedEvent } from "../api";
import { ChatSessionController } from "../ChatSessionController";
import {
  initialChatSessionState,
  reduceChatSessionEvent,
  type ChatSessionState,
} from "../ChatSessionReducer";
import { useChatSessionStore } from "../ChatSessionStore";
import * as presentation from "../ChatTranscriptPresentation";
import { MessageList, type ChatMessage } from "../MessageList";
import { MessageMarkdown } from "../MessageMarkdown";
import {
  streamingFence,
  syntheticTranscript,
  syntheticWireTranscript,
} from "./transcriptFixtures";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

type Bench = TestContext["bench"];
type Mounted = { root: Root; container: HTMLElement };

const noop = () => undefined;
const NO_CALLS = new Set<string>();
const NO_ERRORS: Record<string, string> = {};
const scrollRef = { current: null };

/** One streamed event: a fixed count of renders, so every run does the same work. */
const EVENT_RUN = {
  time: 0,
  iterations: 60,
  warmupTime: 0,
  warmupIterations: 5,
};

function list(messages: ChatMessage[], busy: boolean): ReactNode {
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
      busy={busy}
      // The typewriter's own timer would render outside the measured frame.
      animateStreaming={false}
      scrollRef={scrollRef}
      onScroll={noop}
      onApproval={noop}
      onFolderAccessDecision={noop}
      onFolderAccessCancel={noop}
    />
  );
}

function mount(node: ReactNode): Mounted {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  act(() => root.render(node));
  return { root, container };
}

function unmount(mounted: Mounted | null) {
  if (!mounted) return;
  act(() => mounted.root.unmount());
  mounted.container.remove();
}

/** A long transcript of five-row turns whose last answer is still streaming. */
function streamingTranscript(size: number): ChatMessage[] {
  return syntheticTranscript({
    turns: Math.floor(size / 5),
    toolsPerTurn: 3,
    answerBytes: 1_200,
  });
}

/** What the reducer does with one text delta: a new array, a new tail. */
function appendToLastAssistant(
  messages: ChatMessage[],
  chunk: string,
): ChatMessage[] {
  const last = messages[messages.length - 1]!;
  if (last.role !== "assistant") throw new Error("expected an assistant tail");
  return [...messages.slice(0, -1), { ...last, text: last.text + chunk }];
}

function textDelta(bench: Bench, size: number) {
  let mounted: Mounted | null = null;
  let messages: ChatMessage[] = [];
  let step = 0;
  return bench(
    `${size} messages`,
    {
      beforeAll: () => {
        messages = streamingTranscript(size);
        mounted = mount(list(messages, true));
      },
      afterAll: () => {
        unmount(mounted);
        mounted = null;
      },
    },
    () => {
      step += 1;
      messages = appendToLastAssistant(messages, ` word${step}`);
      act(() => mounted!.root.render(list(messages, true)));
    },
  );
}

test("MessageList, one streamed text delta", async ({ bench }) => {
  await bench.compare(
    textDelta(bench, 600),
    textDelta(bench, 2_400),
    EVENT_RUN,
  );
});

test("MessageList, one tool-argument delta", async ({ bench }) => {
  const size = 2_400;
  let mounted: Mounted | null = null;
  let state: ChatSessionState = initialChatSessionState();
  let seq = 0;
  const deps = { nextId: () => `id-${seq}`, now: () => "2026-09-01T00:00:00Z" };
  await bench(
    `${size} messages`,
    {
      beforeAll: () => {
        const messages: ChatMessage[] = [
          ...streamingTranscript(size),
          {
            id: "live-tool",
            role: "tool",
            callId: "live-call",
            name: "write_file",
            status: "running",
          },
        ];
        state = { ...initialChatSessionState(), busy: true, messages };
        seq = 0;
        mounted = mount(list(state.messages, true));
      },
      afterAll: () => {
        unmount(mounted);
        mounted = null;
      },
    },
    () => {
      seq += 1;
      const frame: SequencedEvent = {
        seq,
        event: { type: "tool_call_args_delta", call_id: "live-call" },
      };
      state = reduceChatSessionEvent(state, frame, deps).state;
      act(() => mounted!.root.render(list(state.messages, true)));
    },
  ).run(EVENT_RUN);
});

function fenceTick(bench: Bench, lines: number) {
  let mounted: Mounted | null = null;
  let text = "";
  let step = 0;
  // Spread so the prop reads the same before and after it existed.
  const render = () => (
    <MessageMarkdown {...{ streaming: true }}>{text}</MessageMarkdown>
  );
  return bench(
    `${lines}-line fence`,
    {
      beforeAll: () => {
        text = streamingFence(lines);
        step = 0;
        mounted = mount(render());
      },
      afterAll: () => {
        unmount(mounted);
        mounted = null;
      },
    },
    () => {
      step += 1;
      // A typewriter tick reveals a few characters; every fourth one ends the
      // line it was typing.
      text += step % 4 === 0 ? "ok);\n" : ` a${step}`;
      act(() => mounted!.root.render(render()));
    },
  );
}

test("Streaming a trailing code fence, one tick", async ({ bench }) => {
  await bench.compare(fenceTick(bench, 200), fenceTick(bench, 600), EVENT_RUN);
});

/** The frames a reconnect replays for a turn that is still running. */
function replayedTurn(firstSeq: number, deltas: number): SequencedEvent[] {
  const frames: SequencedEvent[] = [];
  let seq = firstSeq;
  const push = (event: SequencedEvent["event"]) => {
    frames.push({ seq, event, replayed: true });
    seq += 1;
  };
  push({ type: "turn_started", turn_id: "replayed-turn" });
  for (let call = 0; call < 3; call += 1) {
    push({ type: "tool_call_started", call_id: `r-${call}`, name: "search" });
    for (let arg = 0; arg < 40; arg += 1) {
      push({ type: "tool_call_args_delta", call_id: `r-${call}` });
    }
    push({
      type: "tool_call_completed",
      call_id: `r-${call}`,
      status: "completed",
    });
  }
  for (let delta = 0; delta < deltas; delta += 1) {
    push({ type: "text_delta", text: `tok${delta} ` });
  }
  return frames;
}

/**
 * The route's wiring, reduced to what a replay touches: the controller hands
 * frames to the store, and the transcript re-renders on each publish. Each
 * frame is its own socket task, so each is its own act.
 */
function ReplayedTranscript() {
  const messages = useChatSessionStore((session) => session.messages);
  const busy = useChatSessionStore((session) => session.busy);
  return list(messages, busy);
}

test("Replaying an active turn after a reconnect", async ({ bench }) => {
  const hydrated = streamingTranscript(600);
  const frames = replayedTurn(1, 1_000);
  const deps = { nextId: () => crypto.randomUUID(), now: () => "2026-09-01" };
  let mounted: Mounted | null = null;
  let publishes = 0;
  const unsubscribe = useChatSessionStore.subscribe(() => {
    publishes += 1;
  });
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  await bench(
    `${frames.length} frames onto 600 messages`,
    {
      beforeAll: () => {
        mounted = mount(<ReplayedTranscript />);
      },
      beforeEach: () => {
        act(() => {
          useChatSessionStore.getState().reset();
          useChatSessionStore
            .getState()
            .update((session) => ({ ...session, messages: hydrated }));
        });
        publishes = 0;
      },
      afterAll: () => {
        unmount(mounted);
        mounted = null;
      },
    },
    () => {
      let deliver: (frame: ChatFrame) => void = noop;
      const socket = { close: noop } as unknown as WebSocket;
      const controller = new ChatSessionController({
        openSocket: (_after, onFrame) => {
          deliver = onFrame;
          return socket;
        },
        getAfter: () => useChatSessionStore.getState().lastSeq,
        onEvents: (events) => {
          useChatSessionStore.getState().applyEvents(events, deps);
        },
        onMetadata: noop,
        onConnectionState: noop,
      });
      controller.start();
      for (const frame of frames) act(() => deliver(frame));
      act(() => {
        vi.advanceTimersByTime(1_000);
      });
      controller.dispose();
    },
  ).run({ time: 0, iterations: 3, warmupTime: 0, warmupIterations: 0 });
  vi.useRealTimers();
  unsubscribe();
  console.info(
    `Replay: ${publishes} store publishes for ${frames.length} frames`,
  );
});

test("Opening a chat", async ({ bench }) => {
  const messages = syntheticTranscript({
    turns: 100,
    toolsPerTurn: 2,
    answerBytes: 5_000,
  });
  let mounted: Mounted | null = null;
  let elements = 0;
  await bench(
    "100 turns, 5 KB answers: render and commit",
    {
      afterEach: () => {
        elements = mounted!.container.getElementsByTagName("*").length;
        unmount(mounted);
        mounted = null;
      },
    },
    () => {
      mounted = mount(list(messages, false));
    },
  ).run({ time: 0, iterations: 8, warmupTime: 0, warmupIterations: 2 });
  console.info(`Opening a chat: ${elements} DOM elements after commit`);
});

test("Opening a chat: read, present, render, and commit", async ({ bench }) => {
  // What opening reads: one page where the app pages, all of it where not.
  const pageTurns =
    (presentation as { TRANSCRIPT_PAGE_TURNS?: number })
      .TRANSCRIPT_PAGE_TURNS ?? 100;
  const body = JSON.stringify(
    syntheticWireTranscript({
      turns: 100,
      toolsPerTurn: 2,
      answerBytes: 5_000,
      pageTurns,
    }),
  );
  let mounted: Mounted | null = null;
  await bench(
    `100 turns, 5 KB answers, reading ${pageTurns}`,
    {
      afterEach: () => {
        unmount(mounted);
        mounted = null;
      },
    },
    () => {
      const presented = presentation.presentChatTranscript(JSON.parse(body));
      mounted = mount(list(presented.messages, false));
    },
  ).run({ time: 0, iterations: 8, warmupTime: 0, warmupIterations: 2 });
});
