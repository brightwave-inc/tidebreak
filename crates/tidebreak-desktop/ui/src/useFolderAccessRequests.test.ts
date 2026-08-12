// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient } from "./api";
import { useFolderAccessRequests } from "./useFolderAccessRequests";
import { useNativePickerLatch } from "./NativePickerLatch";
import { usePendingPrompts } from "./PendingPrompts";
import * as host from "./host";

vi.mock("./host", () => ({
  hasNativeHost: vi.fn(() => true),
  resolveFolderAccessRequest: vi.fn(),
}));

function request(callId: string) {
  return { callId, turnId: "turn-1", displayPath: "~/Notes" };
}

function stubClient(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    cancel: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

/**
 * The requests themselves are the shell watcher's job, so a responder test puts
 * them where the watcher would have.
 */
function seedRequests(chatId: string, requests: ReturnType<typeof request>[]) {
  usePendingPrompts.setState({
    chatId,
    folderAccess: requests as never,
    refresh: vi.fn(),
  });
}

beforeEach(() => {
  vi.mocked(host.hasNativeHost).mockReturnValue(true);
  vi.mocked(host.resolveFolderAccessRequest).mockReset();
  // The latch is app-wide, so a decision left open by one case would block
  // the next one.
  useNativePickerLatch.setState({ holder: null });
  usePendingPrompts.setState({ chatId: null, userQuestions: [], folderAccess: [] });
});

afterEach(cleanup);

describe("useFolderAccessRequests", () => {
  it("allows only one decision at a time, since each opens a native dialog", async () => {
    let release: (() => void) | undefined;
    vi.mocked(host.resolveFolderAccessRequest).mockImplementation(
      () => new Promise<void>((resolve) => (release = resolve)),
    );
    const client = stubClient();
    const { result } = renderHook(() =>
      useFolderAccessRequests(client, "chat-1"),
    );

    act(() => result.current.decide("call-1", "allow"));
    await waitFor(() => expect(result.current.resolving.size).toBe(1));

    act(() => result.current.decide("call-2", "allow"));
    expect(host.resolveFolderAccessRequest).toHaveBeenCalledTimes(1);
    expect(result.current.resolving.has("call-2")).toBe(false);

    await act(async () => {
      release?.();
    });
    await waitFor(() => expect(result.current.resolving.size).toBe(0));
  });

  it("does not decide without a native host to ask", async () => {
    vi.mocked(host.hasNativeHost).mockReturnValue(false);
    const client = stubClient();
    const { result } = renderHook(() =>
      useFolderAccessRequests(client, "chat-1"),
    );

    act(() => result.current.decide("call-1", "allow"));

    expect(host.resolveFolderAccessRequest).not.toHaveBeenCalled();
  });

  it("reports a failed decision against its own request", async () => {
    vi.mocked(host.resolveFolderAccessRequest).mockRejectedValue(
      new Error("Folder is unavailable"),
    );
    const client = stubClient();
    const { result } = renderHook(() =>
      useFolderAccessRequests(client, "chat-1"),
    );

    await act(async () => result.current.decide("call-1", "allow"));

    await waitFor(() =>
      expect(result.current.errors["call-1"]).toContain(
        "Folder is unavailable",
      ),
    );
  });

  it("keeps the native picker latched across a conversation switch", async () => {
    // The picker is one shared host resource. Leaving the chat that opened it
    // must not hand a second decision a fresh claim, or the host rejects it and
    // the reader sees an error where the control should have read as blocked.
    let openPicker!: () => void;
    vi.mocked(host.resolveFolderAccessRequest).mockReturnValue(
      new Promise<void>((resolve) => {
        openPicker = resolve;
      }),
    );

    const firstClient = stubClient();
    const first = renderHook(() =>
      useFolderAccessRequests(firstClient, "chat-1"),
    );
    act(() => first.result.current.decide("call-1", "allow"));
    await waitFor(() =>
      expect(first.result.current.resolving.has("call-1")).toBe(true),
    );

    // The reader switches conversations while the picker is still open.
    first.unmount();
    const secondClient = stubClient();
    const second = renderHook(() =>
      useFolderAccessRequests(secondClient, "chat-2"),
    );

    expect(second.result.current.resolving.size).toBe(1);
    act(() => second.result.current.decide("call-2", "allow"));

    expect(host.resolveFolderAccessRequest).toHaveBeenCalledTimes(1);
    expect(host.resolveFolderAccessRequest).not.toHaveBeenCalledWith(
      "chat-2",
      "call-2",
      "allow",
    );

    await act(async () => {
      openPicker();
    });
    expect(second.result.current.resolving.size).toBe(0);
  });

  it("does not carry a decision error into the next conversation", async () => {
    const client = stubClient();
    vi.mocked(host.resolveFolderAccessRequest).mockRejectedValue(
      new Error("no such folder"),
    );
    seedRequests("chat-1", [request("call-1")]);
    const { result, rerender } = renderHook(
      ({ chatId }) => useFolderAccessRequests(client, chatId),
      { initialProps: { chatId: "chat-1" } },
    );
    await waitFor(() => expect(result.current.requests).toHaveLength(1));

    await act(async () => {
      result.current.decide("call-1", "allow");
    });
    await waitFor(() => expect(result.current.errors).not.toEqual({}));

    rerender({ chatId: "chat-2" });

    expect(result.current.errors).toEqual({});
  });
});
