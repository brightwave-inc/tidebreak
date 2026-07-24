// @vitest-environment jsdom
import { createRef } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentRun, ApiClient, Chat, PendingUserQuestions } from "./api";
import { useChatSessionStore } from "./ChatSessionStore";
import {
  ConversationRequestsProvider,
  useConversationRequests,
  type ChatSelectionFence,
  type ConversationRequests,
  type ConversationRequestsHandle,
} from "./ConversationRequests";

const chat = { id: "chat-1", title: "Roadmap" } as unknown as Chat;

const question: PendingUserQuestions = {
  callId: "call-1",
  turnId: "turn-1",
  questions: [{ id: "q1", question: "Which environment?", options: [] }],
} as unknown as PendingUserQuestions;

const sandboxRun: AgentRun = {
  id: "run-1",
  execution: "sandbox",
  status: "running",
  activity: null,
} as unknown as AgentRun;

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  // Nothing observes the rejection until the component's catch runs.
  promise.catch(() => {});
  return { promise, resolve, reject };
}

function fakeClient(overrides: Partial<ApiClient> = {}): ApiClient {
  return {
    listAgentRuns: vi.fn(async () => []),
    listPendingFolderAccessRequests: vi.fn(async () => []),
    listPendingUserQuestions: vi.fn(async () => []),
    ...overrides,
  } as unknown as ApiClient;
}

type Seen = { current: ConversationRequests | null };

/** Renders the published values so assertions read the context, not internals. */
function RequestsProbe({ seen }: { seen: Seen }) {
  const requests = useConversationRequests();
  seen.current = requests;
  return (
    <dl>
      <dd data-testid="questions">
        {requests.userQuestionRequests.map((r) => r.callId).join(",")}
      </dd>
      <dd data-testid="question-errors">
        {Object.values(requests.userQuestionErrors).join(",")}
      </dd>
      <dd data-testid="approval-errors">
        {Object.values(requests.approvalErrors).join(",")}
      </dd>
      <dd data-testid="runs">
        {requests.agentRuns.map((run) => run.id).join(",")}
      </dd>
      <dd data-testid="stop-errors">
        {[...requests.stopErrorRunIds].join(",")}
      </dd>
      <dd data-testid="steer-status">{requests.steerStatus ?? ""}</dd>
      <dd data-testid="cancel-pending">{requests.cancelPendingTurnId ?? ""}</dd>
      <button
        type="button"
        onClick={() => requests.answerUserQuestions("call-1", [])}
      >
        answer
      </button>
      <button type="button" onClick={() => requests.stopSandboxRun("run-1")}>
        stop run
      </button>
      <button
        type="button"
        onClick={() => requests.decideApproval("call-1", "approve")}
      >
        approve
      </button>
      <button type="button" onClick={() => void requests.steerActiveTurn()}>
        steer
      </button>
      <button type="button" onClick={() => void requests.cancelActiveTurn()}>
        stop turn
      </button>
    </dl>
  );
}

function renderRequests(client: ApiClient, draft = "") {
  const selection: ChatSelectionFence = {
    selection: { current: 1 },
    chatId: { current: chat.id },
    deleting: { current: false },
  };
  const handle = createRef<ConversationRequestsHandle>();
  const seen: Seen = { current: null };
  const tree = (open: Chat) => (
    // Keyed by chat: the conversation's requests belong to the conversation,
    // so selecting another one replaces them rather than resetting them.
    <ConversationRequestsProvider
      key={open.id}
      client={client}
      chat={open}
      selection={selection}
      draftRef={{ current: draft }}
      onDraftAccepted={vi.fn()}
      ref={handle}
    >
      <RequestsProbe seen={seen} />
    </ConversationRequestsProvider>
  );
  const { rerender } = render(tree(chat));
  const selectChat = (next: Chat) => {
    selection.selection.current += 1;
    selection.chatId.current = next.id;
    rerender(tree(next));
  };
  return { selection, handle, seen, selectChat };
}

function beginTurn(turnId: string) {
  act(() => {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      busy: true,
      activeTurnId: turnId,
    }));
  });
}

/** Lets the mount-time polls settle so assertions see their first result. */
async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

beforeEach(() => {
  useChatSessionStore.getState().reset();
});
afterEach(cleanup);

describe("ConversationRequestsProvider", () => {
  it("publishes what the conversation is waiting on", async () => {
    renderRequests(
      fakeClient({
        listPendingUserQuestions: vi.fn(async () => [question]),
        listAgentRuns: vi.fn(async () => [sandboxRun]),
      }),
    );
    await settle();

    expect(screen.getByTestId("questions")).toHaveTextContent("call-1");
    expect(screen.getByTestId("runs")).toHaveTextContent("run-1");
  });

  it("discards a failed answer once the selection has moved on", async () => {
    const answer = deferred<void>();
    const { selection } = renderRequests(
      fakeClient({
        listPendingUserQuestions: vi.fn(async () => [question]),
        answerUserQuestions: vi.fn(() => answer.promise),
      }),
    );
    await settle();

    act(() => screen.getByText("answer").click());
    // The user selects another chat while the answer is still in flight.
    selection.selection.current += 1;
    await act(async () => {
      answer.reject(new Error("gone"));
      await answer.promise.catch(() => {});
    });

    expect(screen.getByTestId("question-errors")).toHaveTextContent("");
  });

  it("drops an in-flight sandbox stop when the chat is being deleted", async () => {
    const stop = deferred<never>();
    const { handle } = renderRequests(
      fakeClient({
        listAgentRuns: vi.fn(async () => [sandboxRun]),
        cancelAgentRun: vi.fn(() => stop.promise),
      }),
    );
    await settle();

    act(() => screen.getByText("stop run").click());
    act(() => handle.current?.abandonForChatDeletion());
    await act(async () => {
      stop.reject(new Error("gone"));
      await stop.promise.catch(() => {});
    });

    expect(screen.getByTestId("stop-errors")).toHaveTextContent("");
  });

  it("clears steer and cancel state once the turn resolves", async () => {
    const steer = deferred<void>();
    const { handle } = renderRequests(
      fakeClient({
        steer: vi.fn(() => steer.promise),
        cancel: vi.fn(async () => {}),
      }),
      "try the other branch",
    );
    await settle();
    beginTurn("turn-1");

    act(() => screen.getByText("steer").click());
    await act(async () => {
      steer.resolve();
      await steer.promise;
    });
    act(() => screen.getByText("stop turn").click());

    expect(screen.getByTestId("steer-status")).toHaveTextContent(
      "Guidance sent",
    );
    expect(screen.getByTestId("cancel-pending")).toHaveTextContent("turn-1");

    act(() => handle.current?.turnResolved());

    expect(screen.getByTestId("steer-status")).toHaveTextContent("");
    expect(screen.getByTestId("cancel-pending")).toHaveTextContent("");
  });

  it("discards a decision that lands after another chat is selected", async () => {
    const decision = deferred<void>();
    const { selectChat } = renderRequests(
      fakeClient({ decideApproval: vi.fn(() => decision.promise) }),
    );
    await settle();

    act(() => screen.getByText("approve").click());
    act(() => selectChat({ ...chat, id: "chat-2" } as Chat));
    await act(async () => {
      decision.reject(new Error("gone"));
      await decision.promise.catch(() => {});
    });
    await settle();

    expect(screen.getByTestId("approval-errors")).toHaveTextContent("");
  });

  it("settles a steer only once the guidance has been accepted", async () => {
    // The composer awaits this to decide where focus belongs afterwards, so
    // resolving on submit would restore focus while the request is still open.
    const steer = deferred<void>();
    const { seen } = renderRequests(
      fakeClient({ steer: vi.fn(() => steer.promise) }),
      "try the other branch",
    );
    await settle();
    beginTurn("turn-1");

    let settled = false;
    let sending!: Promise<void>;
    act(() => {
      sending = seen.current!.steerActiveTurn().then(() => {
        settled = true;
      });
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(settled).toBe(false);

    await act(async () => {
      steer.resolve();
      await sending;
    });

    expect(settled).toBe(true);
  });
});
