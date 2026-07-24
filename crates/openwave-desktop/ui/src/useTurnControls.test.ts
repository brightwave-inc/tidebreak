// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient } from "./api";
import { useChatListStore } from "./ChatListStore";
import { useChatSessionStore } from "./ChatSessionStore";
import { useTurnControls } from "./useTurnControls";
import { useTurnLifecycle } from "./TurnLifecycleSignals";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  promise.catch(() => {});
  return { promise, resolve, reject };
}

function stubClient(overrides: Record<string, unknown> = {}) {
  return {
    cancel: vi.fn().mockResolvedValue(undefined),
    steer: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

/** Puts the session store where a turn is running, as the composer sees it. */
function runTurn(turnId = "turn-1") {
  useChatSessionStore
    .getState()
    .update((session) => ({ ...session, busy: true, activeTurnId: turnId }));
}

function mount(client: ApiClient, draft = "change course", chatId = "chat-1") {
  const draftRef = { current: draft };
  const onDraftAccepted = vi.fn(() => {
    draftRef.current = "";
  });
  const view = renderHook(
    ({ id }) => useTurnControls(client, id, draftRef, onDraftAccepted),
    { initialProps: { id: chatId } },
  );
  return { ...view, draftRef, onDraftAccepted };
}

beforeEach(() => {
  useChatSessionStore.getState().reset();
});

afterEach(() => {
  cleanup();
  useChatListStore.getState().setDeletingChatId(null);
});

describe("useTurnControls", () => {
  it("settles the steer promise when the request settles, not when it is sent", async () => {
    // The composer awaits this to time restoring focus. Resolving on submit
    // would hand focus back while the request is still open, and would make the
    // composer's own stale-submission guard unreachable.
    const request = deferred<void>();
    const client = stubClient({ steer: vi.fn(() => request.promise) });
    const { result } = mount(client);
    act(() => runTurn());

    let settled = false;
    let steering!: Promise<void>;
    act(() => {
      steering = result.current.steer().then(() => {
        settled = true;
      });
    });
    await waitFor(() =>
      expect(result.current.steerPendingTurnId).toBe("turn-1"),
    );
    expect(settled).toBe(false);

    await act(async () => {
      request.resolve();
      await steering;
    });

    expect(settled).toBe(true);
    expect(result.current.steerStatus).toBe("Guidance sent");
  });

  it("sends the live draft and clears it once the guidance is accepted", async () => {
    const client = stubClient();
    const { result, draftRef, onDraftAccepted } = mount(client, "  go left  ");
    act(() => runTurn());

    await act(async () => result.current.steer());

    expect(client.steer).toHaveBeenCalledWith(
      "chat-1",
      "turn-1",
      expect.any(String),
      "go left",
      true,
    );
    expect(onDraftAccepted).toHaveBeenCalledTimes(1);
    expect(draftRef.current).toBe("");
    expect(result.current.steerPendingTurnId).toBeNull();
  });

  it("keeps a draft the reader has since changed", async () => {
    const request = deferred<void>();
    const client = stubClient({ steer: vi.fn(() => request.promise) });
    const { result, draftRef, onDraftAccepted } = mount(client, "go left");
    act(() => runTurn());

    const steering = result.current.steer();
    draftRef.current = "go left, then right";
    await act(async () => {
      request.resolve();
      await steering;
    });

    expect(onDraftAccepted).not.toHaveBeenCalled();
    expect(result.current.steerStatus).toBe("Guidance sent");
  });

  it("reports a failed steer and lets the reader retry it", async () => {
    const client = stubClient({
      steer: vi.fn().mockRejectedValue(new Error("steer rejected")),
    });
    const { result } = mount(client);
    act(() => runTurn());

    await act(async () => result.current.steer());

    expect(result.current.steerError).toContain("steer rejected");
    expect(result.current.steerStatus).toBeNull();
    expect(result.current.steerPendingTurnId).toBeNull();

    await act(async () => result.current.steer());
    expect(client.steer).toHaveBeenCalledTimes(2);
    // Unchanged guidance keeps its identity, so a retry cannot be applied twice.
    expect(vi.mocked(client.steer).mock.calls[1]?.[2]).toBe(
      vi.mocked(client.steer).mock.calls[0]?.[2],
    );
  });

  it("clears the steer verdict without disturbing a request in flight", async () => {
    const request = deferred<void>();
    const client = stubClient({ steer: vi.fn(() => request.promise) });
    const { result } = mount(client);
    act(() => runTurn());

    act(() => void result.current.steer());
    await waitFor(() =>
      expect(result.current.steerStatus).toBe("Sending guidance…"),
    );

    act(() => result.current.clearSteerFeedback());

    expect(result.current.steerStatus).toBeNull();
    expect(result.current.steerError).toBeNull();
    expect(result.current.steerPendingTurnId).toBe("turn-1");
    await act(async () => {
      request.resolve();
      await request.promise;
    });
  });

  it("drops a failed steer once its chat is being deleted", async () => {
    const request = deferred<void>();
    const client = stubClient({ steer: vi.fn(() => request.promise) });
    const { result } = mount(client);
    act(() => runTurn());

    act(() => void result.current.steer());
    await waitFor(() =>
      expect(result.current.steerPendingTurnId).toBe("turn-1"),
    );

    // The chat keeps its id for the whole delete round trip, which is why a
    // plain id comparison misses this.
    act(() => useChatListStore.getState().setDeletingChatId("chat-1"));
    await act(async () => {
      request.reject(new Error("gone"));
      await request.promise.catch(() => {});
    });

    expect(result.current.steerError).toBeNull();
    expect(result.current.steerStatus).toBeNull();
    expect(result.current.steerPendingTurnId).toBeNull();
  });

  it("does not report a steer onto the conversation that replaced it", async () => {
    // The chat pane is keyed on the chat, so in the app this reply lands on an
    // unmounted hook. The chat id in the request's identity is what makes that
    // safe rather than incidental.
    const request = deferred<void>();
    const client = stubClient({ steer: vi.fn(() => request.promise) });
    const { result, rerender, onDraftAccepted } = mount(client);
    act(() => runTurn());

    act(() => void result.current.steer());
    await waitFor(() =>
      expect(result.current.steerPendingTurnId).toBe("turn-1"),
    );

    rerender({ id: "chat-2" });
    await act(async () => {
      request.resolve();
      await request.promise;
    });

    expect(result.current.steerStatus).not.toBe("Guidance sent");
    expect(onDraftAccepted).not.toHaveBeenCalled();
  });

  it("refuses guidance while a cancel for the turn is outstanding", async () => {
    const cancelling = deferred<void>();
    const client = stubClient({ cancel: vi.fn(() => cancelling.promise) });
    const { result } = mount(client);
    act(() => runTurn());

    act(() => void result.current.cancel());
    await waitFor(() =>
      expect(result.current.cancelPendingTurnId).toBe("turn-1"),
    );
    await act(async () => result.current.steer());

    expect(client.steer).not.toHaveBeenCalled();
    await act(async () => {
      cancelling.resolve();
      await cancelling.promise;
    });
  });

  it("refuses a second cancel for the turn it already asked to stop", async () => {
    const cancelling = deferred<void>();
    const client = stubClient({ cancel: vi.fn(() => cancelling.promise) });
    const { result } = mount(client);
    act(() => runTurn());

    act(() => void result.current.cancel());
    await waitFor(() =>
      expect(result.current.cancelPendingTurnId).toBe("turn-1"),
    );
    await act(async () => result.current.cancel());

    expect(client.cancel).toHaveBeenCalledTimes(1);
    await act(async () => {
      cancelling.resolve();
      await cancelling.promise;
    });
  });

  it("reports a failed cancel and reopens the control", async () => {
    const client = stubClient({
      cancel: vi.fn().mockRejectedValue(new Error("already finished")),
    });
    const { result } = mount(client);
    act(() => runTurn());

    await act(async () => result.current.cancel());

    expect(result.current.cancelError).toContain("already finished");
    expect(result.current.cancelPendingTurnId).toBeNull();

    await act(async () => result.current.cancel());
    expect(client.cancel).toHaveBeenCalledTimes(2);
  });

  it("drops a failed cancel once its chat is being deleted", async () => {
    const cancelling = deferred<void>();
    const client = stubClient({ cancel: vi.fn(() => cancelling.promise) });
    const { result } = mount(client);
    act(() => runTurn());

    act(() => void result.current.cancel());
    await waitFor(() =>
      expect(result.current.cancelPendingTurnId).toBe("turn-1"),
    );

    act(() => useChatListStore.getState().setDeletingChatId("chat-1"));
    await act(async () => {
      cancelling.reject(new Error("gone"));
      await cancelling.promise.catch(() => {});
    });

    expect(result.current.cancelError).toBeNull();
  });

  it("does not steer a conversation that is being deleted", async () => {
    const client = stubClient();
    const { result } = mount(client);
    act(() => runTurn());

    act(() => useChatListStore.getState().setDeletingChatId("chat-2"));
    await act(async () => result.current.steer());

    expect(client.steer).not.toHaveBeenCalled();
  });

  it("retires guidance aimed at a conversation being deleted", async () => {
    const client = stubClient();
    const { result } = mount(client);
    act(() => runTurn());
    await act(async () => result.current.steer());
    expect(result.current.steerStatus).toBe("Guidance sent");

    act(() => useChatListStore.getState().setDeletingChatId("chat-1"));

    await waitFor(() => expect(result.current.steerStatus).toBeNull());
  });

  it("lets the reader steer again once a failed deletion releases the turn", async () => {
    const request = deferred<void>();
    const client = stubClient({ steer: vi.fn(() => request.promise) });
    const { result } = mount(client);
    act(() => runTurn());

    act(() => void result.current.steer());
    act(() => useChatListStore.getState().setDeletingChatId("chat-1"));
    await act(async () => {
      request.resolve();
      await request.promise;
    });
    // The delete failed, so the conversation the reader was guiding is still
    // here — and guidance the fence never released could never be sent again.
    act(() => useChatListStore.getState().setDeletingChatId(null));

    await act(async () => result.current.steer());

    expect(client.steer).toHaveBeenCalledTimes(2);
  });
});

describe("useTurnControls turn lifecycle", () => {
  /** Sends guidance and asks to stop, so both controls have state to retire. */
  async function armBothControls(client: ApiClient) {
    const view = mount(client);
    act(() => runTurn());
    await act(async () => view.result.current.steer());
    await act(async () => view.result.current.cancel());
    expect(view.result.current.steerStatus).toBe("Guidance sent");
    return view;
  }

  it("retires the cancel and the guidance when a different turn begins", async () => {
    const client = stubClient();
    const { result } = await armBothControls(client);

    act(() => useTurnLifecycle.getState().signal("began"));

    expect(result.current.cancelPendingTurnId).toBeNull();
    expect(result.current.cancelError).toBeNull();
    expect(result.current.steerStatus).toBeNull();
    expect(result.current.steerPendingTurnId).toBeNull();

    // The cancel-request turn went with it, so the control is live again.
    await act(async () => result.current.cancel());
    expect(client.cancel).toHaveBeenCalledTimes(2);
  });

  it("keeps standing guidance when the same turn is re-announced", async () => {
    const client = stubClient();
    const { result } = await armBothControls(client);

    act(() => useTurnLifecycle.getState().signal("began_same_turn"));

    // The turn the reader guided is still running, so the notice stands.
    expect(result.current.steerStatus).toBe("Guidance sent");
    expect(result.current.cancelPendingTurnId).toBeNull();
    await act(async () => result.current.cancel());
    expect(client.cancel).toHaveBeenCalledTimes(2);
  });

  it("retires both controls when the turn resolves", async () => {
    const client = stubClient();
    const { result } = await armBothControls(client);

    act(() => useTurnLifecycle.getState().signal("resolved"));

    expect(result.current.cancelPendingTurnId).toBeNull();
    expect(result.current.steerStatus).toBeNull();
    await act(async () => result.current.cancel());
    expect(client.cancel).toHaveBeenCalledTimes(2);
  });

  it("leaves the cancel-request turn standing after a local submission", async () => {
    const client = stubClient();
    const { result } = await armBothControls(client);

    act(() => useTurnLifecycle.getState().signal("submitted"));

    // What the reader can see is cleared, but the turn already asked to stop is
    // still fenced: only the server confirming a turn retires that.
    expect(result.current.cancelPendingTurnId).toBeNull();
    expect(result.current.cancelError).toBeNull();
    expect(result.current.steerStatus).toBe("Guidance sent");
    await act(async () => result.current.cancel());
    expect(client.cancel).toHaveBeenCalledTimes(1);
  });

  it("reacts once per signal, so a repeated event is a repeated reaction", async () => {
    const client = stubClient();
    const { result, draftRef } = mount(client);
    act(() => runTurn());

    for (const _attempt of [1, 2]) {
      // Accepting the first attempt emptied the composer; the reader types the
      // next piece of guidance before sending it.
      draftRef.current = "change course";
      await act(async () => result.current.steer());
      expect(result.current.steerStatus).toBe("Guidance sent");
      act(() => useTurnLifecycle.getState().signal("resolved"));
      expect(result.current.steerStatus).toBeNull();
    }
  });
});
