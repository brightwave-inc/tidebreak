// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { Chat } from "./api";
import * as host from "./host";
import { useNativePickerLatch } from "./NativePickerLatch";
import { useRefreshSignals } from "./RefreshSignals";
import { useChatFolderAttachments } from "./useChatFolderAttachments";

vi.mock("./host", () => ({
  connectFolder: vi.fn(),
  disconnectFolder: vi.fn(),
  listCapabilityConsents: vi.fn(),
  listConnectedFolders: vi.fn(),
}));

const chat = {
  id: "chat-1",
  project_id: null,
} as unknown as Chat;

function Harness() {
  const folders = useChatFolderAttachments(chat, true);
  return (
    <>
      <button type="button" onClick={folders.attach}>
        Attach
      </button>
      {folders.items.map((folder) => (
        <button
          type="button"
          key={folder.rootId}
          onClick={() => folders.remove(folder.rootId)}
        >
          Remove {folder.displayName}
        </button>
      ))}
    </>
  );
}

beforeEach(() => {
  useNativePickerLatch.setState({ holder: null });
  useRefreshSignals.setState({
    folderAccess: 0,
    outputWritebacks: 0,
    userQuestions: 0,
  });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("connects and revokes a chat folder through the existing host flow", async () => {
  vi.mocked(host.listCapabilityConsents).mockResolvedValue([]);
  vi.mocked(host.listConnectedFolders)
    .mockResolvedValueOnce([])
    .mockResolvedValueOnce([
      {
        rootId: "root-1",
        displayName: "Research",
        status: "connected",
      },
    ])
    .mockResolvedValueOnce([]);
  vi.mocked(host.connectFolder).mockResolvedValue({
    rootId: "root-1",
    displayName: "Research",
    status: "connected",
  });
  vi.mocked(host.disconnectFolder).mockResolvedValue(true);
  const user = userEvent.setup();

  render(<Harness />);
  await waitFor(() => expect(host.listConnectedFolders).toHaveBeenCalledOnce());

  await user.click(screen.getByRole("button", { name: "Attach" }));

  expect(host.connectFolder).toHaveBeenCalledWith(chat);
  expect(
    await screen.findByRole("button", { name: "Remove Research" }),
  ).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Remove Research" }));

  expect(host.disconnectFolder).toHaveBeenCalledWith(chat, "root-1");
  await waitFor(() =>
    expect(
      screen.queryByRole("button", { name: "Remove Research" }),
    ).not.toBeInTheDocument(),
  );
});
