// @vitest-environment jsdom

import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  DocumentDropTarget,
  dropItemCountCopy,
  parseDropState,
} from "./DocumentDropTarget";

const mocks = vi.hoisted(() => ({
  handler: undefined as ((event: { payload: unknown }) => void) | undefined,
  attachDropped: vi.fn(),
  stop: vi.fn(),
  localHostAuthority: vi.fn(() => true),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    async (_event: string, handler: (event: { payload: unknown }) => void) => {
      mocks.handler = handler;
      return mocks.stop;
    },
  ),
}));

vi.mock("./attachments", () => ({
  attachDroppedChatFiles: mocks.attachDropped,
}));

vi.mock("./host", () => ({
  hasNativeHost: () => true,
  hasLocalHostAuthority: mocks.localHostAuthority,
}));

describe("DocumentDropTarget", () => {
  beforeEach(() => {
    mocks.handler = undefined;
    mocks.attachDropped.mockReset();
    mocks.attachDropped.mockResolvedValue({
      images: [],
      documents: null,
      failedImages: [],
    });
    mocks.stop.mockReset();
    mocks.localHostAuthority.mockReturnValue(true);
  });

  it("offers native files and folders without claiming aliases", async () => {
    const onAttached = vi.fn();
    render(
      <DocumentDropTarget
        resolveChatId={async () => "chat-1"}
        onAttached={onAttached}
        onError={vi.fn()}
      />,
    );
    await waitFor(() => expect(mocks.handler).toBeTypeOf("function"));

    act(() => {
      mocks.handler?.({
        payload: { phase: "enter", accepted: true, fileCount: 1 },
      });
    });
    expect(screen.getByText("Attach this file or folder")).toBeInTheDocument();
    expect(dropItemCountCopy(3)).toBe("3 files or folders");

    act(() => {
      mocks.handler?.({
        payload: { phase: "dropped", accepted: true, fileCount: 1 },
      });
    });
    // The conversation is resolved on the way through, so the claim lands a
    // turn later than the drop.
    await waitFor(() => expect(onAttached).toHaveBeenCalledOnce());
    expect(mocks.attachDropped).toHaveBeenCalledWith("chat-1");
  });

  it("cancels its native listener and ignores a stale drop after unmount", async () => {
    const resolveChatId = vi.fn(async () => "chat-1");
    const view = render(
      <DocumentDropTarget
        resolveChatId={resolveChatId}
        onAttached={vi.fn()}
        onError={vi.fn()}
      />,
    );
    await waitFor(() => expect(mocks.handler).toBeTypeOf("function"));
    view.unmount();
    expect(mocks.stop).toHaveBeenCalledOnce();

    act(() => {
      mocks.handler?.({
        payload: { phase: "dropped", accepted: true, fileCount: 1 },
      });
    });
    // Resolving the conversation is what creates one on home, so a drop
    // delivered after teardown must not reach the resolver either.
    expect(resolveChatId).not.toHaveBeenCalled();
    expect(mocks.attachDropped).not.toHaveBeenCalled();
  });

  it("refuses a drop while this window works on another machine", async () => {
    mocks.localHostAuthority.mockReturnValue(false);
    const onAttached = vi.fn();
    const resolveChatId = vi.fn(async () => "chat-1");
    render(
      <DocumentDropTarget
        resolveChatId={resolveChatId}
        onAttached={onAttached}
        onError={vi.fn()}
      />,
    );
    await waitFor(() => expect(mocks.handler).toBeTypeOf("function"));

    act(() => {
      mocks.handler?.({
        payload: { phase: "enter", accepted: true, fileCount: 1 },
      });
    });
    // The reader learns before letting go, and is pointed at the route that
    // does reach the machine.
    expect(
      screen.getByText("These files are on this computer"),
    ).toBeInTheDocument();
    expect(screen.getByText(/Use Attach files/)).toBeInTheDocument();

    act(() => {
      mocks.handler?.({
        payload: { phase: "dropped", accepted: true, fileCount: 1 },
      });
    });
    // The host would import these paths into the store inside this app, where
    // the conversation does not exist, so the claim never goes out.
    expect(resolveChatId).not.toHaveBeenCalled();
    expect(mocks.attachDropped).not.toHaveBeenCalled();
    expect(onAttached).not.toHaveBeenCalled();
  });

  it("rejects malformed drop projections", () => {
    expect(
      parseDropState({ phase: "enter", accepted: true, fileCount: -1 }),
    ).toBeNull();
    expect(
      parseDropState({ phase: "enter", accepted: true, fileCount: 2 }),
    ).toEqual({
      phase: "enter",
      accepted: true,
      fileCount: 2,
    });
  });
});
