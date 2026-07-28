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
  importDropped: vi.fn(),
  stop: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, handler: (event: { payload: unknown }) => void) => {
    mocks.handler = handler;
    return mocks.stop;
  }),
}));

vi.mock("./documents", () => ({
  importDroppedLibraryDocuments: mocks.importDropped,
}));

vi.mock("./host", () => ({
  hasNativeHost: () => true,
}));

describe("DocumentDropTarget", () => {
  beforeEach(() => {
    mocks.handler = undefined;
    mocks.importDropped.mockReset();
    mocks.importDropped.mockResolvedValue(null);
    mocks.stop.mockReset();
  });

  it("offers native files and folders without claiming aliases", async () => {
    render(<DocumentDropTarget chatId="chat-1" />);
    await waitFor(() => expect(mocks.handler).toBeTypeOf("function"));

    act(() => {
      mocks.handler?.({
        payload: { phase: "enter", accepted: true, fileCount: 1 },
      });
    });
    expect(
      screen.getByText("Add this file or folder to this conversation"),
    ).toBeInTheDocument();
    expect(dropItemCountCopy(3)).toBe("3 files or folders");

    act(() => {
      mocks.handler?.({
        payload: { phase: "dropped", accepted: true, fileCount: 1 },
      });
    });
    expect(mocks.importDropped).toHaveBeenCalledWith("chat-1");
  });

  it("cancels its native listener and ignores a stale drop after unmount", async () => {
    const view = render(<DocumentDropTarget chatId="chat-1" />);
    await waitFor(() => expect(mocks.handler).toBeTypeOf("function"));
    view.unmount();
    expect(mocks.stop).toHaveBeenCalledOnce();

    act(() => {
      mocks.handler?.({
        payload: { phase: "dropped", accepted: true, fileCount: 1 },
      });
    });
    expect(mocks.importDropped).not.toHaveBeenCalled();
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
