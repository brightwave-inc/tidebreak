// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import { FoldersView } from "./FoldersView";
import * as host from "./host";

vi.mock("./host", () => ({
  connectApprovedFolder: vi.fn(),
  connectFolder: vi.fn(),
  disconnectFolder: vi.fn(),
  listApprovedFolders: vi.fn(),
  listConnectedFolders: vi.fn(),
}));

const chat = {
  id: "chat-1",
  title: "Folder test",
  project_id: null,
} as unknown as Chat;

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

beforeEach(() => {
  vi.mocked(host.listConnectedFolders).mockResolvedValue([]);
  vi.mocked(host.listApprovedFolders).mockResolvedValue([]);
  vi.mocked(host.connectApprovedFolder).mockResolvedValue(null);
  vi.mocked(host.connectFolder).mockResolvedValue(null);
  vi.mocked(host.disconnectFolder).mockResolvedValue(false);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("FoldersView", () => {
  it("renders an empty chat without treating the missing grant as an error", async () => {
    render(<FoldersView chat={chat} />);

    expect(
      await screen.findByText("No folders connected to this chat."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(host.listConnectedFolders).toHaveBeenCalledWith(chat);
    expect(host.listApprovedFolders).toHaveBeenCalledOnce();
  });

  it("offers approved folders that are not already connected", async () => {
    vi.mocked(host.listConnectedFolders)
      .mockResolvedValueOnce([
        { rootId: "connected", displayName: "Current project" },
      ])
      .mockResolvedValueOnce([
        { rootId: "connected", displayName: "Current project" },
        { rootId: "available", displayName: "Research" },
      ]);
    vi.mocked(host.listApprovedFolders).mockResolvedValue([
      { rootId: "connected", displayName: "Current project" },
      { rootId: "available", displayName: "Research" },
    ]);
    vi.mocked(host.connectApprovedFolder).mockResolvedValue({
      rootId: "available",
      displayName: "Research",
    });
    const user = userEvent.setup();
    render(<FoldersView chat={chat} />);

    expect(await screen.findByText("Available on this Mac")).toBeInTheDocument();
    expect(screen.getByText("Current project")).toBeInTheDocument();
    expect(screen.getByText("Research")).toBeInTheDocument();
    expect(screen.getAllByText("Current project")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "Connect" }));
    expect(host.connectApprovedFolder).toHaveBeenCalledWith(chat, "available");
    await waitFor(() =>
      expect(host.listConnectedFolders).toHaveBeenCalledTimes(2),
    );
    expect(screen.queryByText("Available on this Mac")).not.toBeInTheDocument();
    expect(screen.getAllByText("Research")).toHaveLength(1);
  });

  it("leaves an approved folder unattached when native consent is canceled", async () => {
    vi.mocked(host.listApprovedFolders).mockResolvedValue([
      { rootId: "available", displayName: "Research" },
    ]);
    const user = userEvent.setup();
    render(<FoldersView chat={chat} />);

    await user.click(await screen.findByRole("button", { name: "Connect" }));

    expect(host.connectApprovedFolder).toHaveBeenCalledWith(chat, "available");
    expect(host.listConnectedFolders).toHaveBeenCalledOnce();
    expect(screen.getByText("Research")).toBeInTheDocument();
  });

  it("ignores a late folder response after switching chats", async () => {
    const firstChat = { ...chat, id: "chat-a", title: "Chat A" };
    const secondChat = { ...chat, id: "chat-b", title: "Chat B" };
    const firstResponse = deferred<host.ConnectedFolder[]>();
    const secondResponse = deferred<host.ConnectedFolder[]>();
    vi.mocked(host.listConnectedFolders).mockImplementation(
      (requestedChat) =>
        requestedChat.id === firstChat.id
          ? firstResponse.promise
          : secondResponse.promise,
    );

    const { rerender } = render(<FoldersView chat={firstChat} />);
    rerender(<FoldersView chat={secondChat} />);

    secondResponse.resolve([
      { rootId: "chat-b-root", displayName: "Chat B folder" },
    ]);
    expect(await screen.findByText("Chat B folder")).toBeInTheDocument();

    firstResponse.resolve([
      { rootId: "chat-a-root", displayName: "Chat A folder" },
    ]);
    await waitFor(() =>
      expect(screen.queryByText("Chat A folder")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("Chat B folder")).toBeInTheDocument();
  });
});
