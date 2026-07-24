// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient } from "./api";
import { useFolderAccessRequests } from "./useFolderAccessRequests";
import { useRefreshSignals } from "./RefreshSignals";
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
    listPendingFolderAccessRequests: vi.fn().mockResolvedValue([]),
    cancel: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

beforeEach(() => {
  vi.mocked(host.hasNativeHost).mockReturnValue(true);
  vi.mocked(host.resolveFolderAccessRequest).mockReset();
});

afterEach(cleanup);

describe("useFolderAccessRequests", () => {
  it("loads the conversation's pending requests", async () => {
    const client = stubClient({
      listPendingFolderAccessRequests: vi.fn().mockResolvedValue([
        request("call-1"),
      ]),
    });
    const { result } = renderHook(() =>
      useFolderAccessRequests(client, "chat-1"),
    );

    await waitFor(() => expect(result.current.requests).toHaveLength(1));
    expect(client.listPendingFolderAccessRequests).toHaveBeenCalledWith(
      "chat-1",
    );
  });

  it("refreshes when the event stream signals", async () => {
    const client = stubClient();
    renderHook(() => useFolderAccessRequests(client, "chat-1"));
    await waitFor(() =>
      expect(client.listPendingFolderAccessRequests).toHaveBeenCalledTimes(1),
    );

    act(() => useRefreshSignals.getState().signal("folderAccess"));

    await waitFor(() =>
      expect(client.listPendingFolderAccessRequests).toHaveBeenCalledTimes(2),
    );
  });

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

  it("drops the previous chat's requests when the conversation changes", async () => {
    const client = stubClient({
      listPendingFolderAccessRequests: vi
        .fn()
        .mockResolvedValueOnce([request("call-1")])
        .mockResolvedValue([]),
    });
    const { result, rerender } = renderHook(
      ({ chatId }) => useFolderAccessRequests(client, chatId),
      { initialProps: { chatId: "chat-1" } },
    );
    await waitFor(() => expect(result.current.requests).toHaveLength(1));

    rerender({ chatId: "chat-2" });

    await waitFor(() => expect(result.current.requests).toHaveLength(0));
    expect(client.listPendingFolderAccessRequests).toHaveBeenLastCalledWith(
      "chat-2",
    );
  });
});
