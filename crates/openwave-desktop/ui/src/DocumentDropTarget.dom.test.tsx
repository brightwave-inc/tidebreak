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
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, handler: (event: { payload: unknown }) => void) => {
    mocks.handler = handler;
    return mocks.stop;
  }),
}));

vi.mock("./attachments", () => ({
  attachDroppedChatFiles: mocks.attachDropped,
}));

vi.mock("./host", () => ({
  hasNativeHost: () => true,
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
    expect(
      screen.getByText("Attach this file or folder"),
    ).toBeInTheDocument();
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

  it("rejects malformed drop projections", () => {
    expect(parseDropState({ phase: "enter", accepted: true, fileCount: -1 })).toBeNull();
    expect(parseDropState({ phase: "enter", accepted: true, fileCount: 2 })).toEqual({
      phase: "enter",
      accepted: true,
      fileCount: 2,
    });
  });
});
