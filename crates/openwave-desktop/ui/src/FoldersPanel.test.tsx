// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import { FoldersPanel } from "./FoldersPanel";
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

describe("FoldersPanel", () => {
  it("renders an empty chat without treating the missing grant as an error", async () => {
    render(<FoldersPanel chat={chat} />);

    expect(
      await screen.findByText("No folders connected to this chat."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(host.listConnectedFolders).toHaveBeenCalledWith(chat);
    expect(host.listApprovedFolders).toHaveBeenCalledOnce();
  });

  it("offers approved folders that are not already connected", async () => {
    vi.mocked(host.listConnectedFolders).mockResolvedValue([
      { rootId: "connected", displayName: "Current project" },
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
    render(<FoldersPanel chat={chat} />);

    expect(await screen.findByText("Available on this Mac")).toBeInTheDocument();
    expect(screen.getByText("Current project")).toBeInTheDocument();
    expect(screen.getByText("Research")).toBeInTheDocument();
    expect(screen.getAllByText("Current project")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "Connect" }));
    expect(host.connectApprovedFolder).toHaveBeenCalledWith(chat, "available");
    await waitFor(() =>
      expect(host.listConnectedFolders).toHaveBeenCalledTimes(2),
    );
  });
});
