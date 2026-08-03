// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat, ConsentStatementSnapshot } from "./api";
import { FoldersView } from "./FoldersView";
import * as host from "./host";

vi.mock("./host", () => ({
  connectApprovedFolder: vi.fn(),
  connectFolder: vi.fn(),
  disconnectFolder: vi.fn(),
  forgetFolder: vi.fn(),
  grantFolderCapability: vi.fn(),
  listApprovedFolders: vi.fn(),
  listCapabilityConsents: vi.fn(),
  listConnectedFolders: vi.fn(),
  revokeCapabilityConsent: vi.fn(),
}));

const chat = {
  id: "chat-1",
  title: "Folder test",
  project_id: null,
} as unknown as Chat;

function folder(
  rootId: string,
  displayName: string,
  status: host.FolderStatus = "connected",
): host.ConnectedFolder {
  return { rootId, displayName, status };
}

type FolderCapabilityVerb = Extract<
  ConsentStatementSnapshot["verb"],
  { kind: "capability" }
>["capability"];

let nextGrant = 0;

function statement(
  rootId: string,
  capability: FolderCapabilityVerb,
): ConsentStatementSnapshot {
  nextGrant += 1;
  return {
    handle: { kind: "capability_grant", grant_id: `grant-${nextGrant}` },
    level: { level: "chat", chat_id: chat.id },
    level_title: null,
    verb: { kind: "capability", capability },
    resource: { kind: "host_root", root_id: rootId, display_name: null },
    method: "folder_picker",
    granted_at: "2026-07-30T12:00:00Z",
  };
}

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
  vi.mocked(host.listCapabilityConsents).mockResolvedValue([]);
  vi.mocked(host.connectApprovedFolder).mockResolvedValue(null);
  vi.mocked(host.connectFolder).mockResolvedValue(null);
  vi.mocked(host.disconnectFolder).mockResolvedValue(false);
  vi.mocked(host.forgetFolder).mockResolvedValue(true);
  vi.mocked(host.grantFolderCapability).mockResolvedValue(null);
  vi.mocked(host.revokeCapabilityConsent).mockResolvedValue(true);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("FoldersView", () => {
  it("renders an empty chat without treating the missing grant as an error", async () => {
    render(<FoldersView chat={chat} />);

    expect(await screen.findByText("No folders connected")).toBeInTheDocument();
    // A chat with no grant yet is the ordinary starting state, not a failure.
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(host.listConnectedFolders).toHaveBeenCalledWith(chat);
    expect(host.listApprovedFolders).toHaveBeenCalledOnce();
  });

  // Access is read off the same consent statements the Permissions surface
  // shows, so the badge follows what the broker holds — including command
  // reach, which the retired folder-capability model could not name.
  it("derives each folder's access from its consent statements", async () => {
    vi.mocked(host.listConnectedFolders).mockResolvedValue([
      folder("writable", "Drafts"),
      folder("read-only", "Archive"),
    ]);
    vi.mocked(host.listCapabilityConsents).mockResolvedValue([
      statement("writable", "read_files"),
      statement("writable", "write_files"),
      statement("writable", "execute_commands"),
      statement("read-only", "read_files"),
      // Another chat's statement must not color this chat's badge.
      {
        ...statement("read-only", "write_files"),
        level: { level: "chat", chat_id: "someone-else" },
      },
    ]);
    render(<FoldersView chat={chat} />);

    expect(
      await screen.findByText("Read, write, and commands"),
    ).toBeInTheDocument();
    expect(screen.getByText("Read only")).toBeInTheDocument();
    // Each held statement is its own row, revocable in place; what a folder
    // is missing gets a grant affordance instead.
    expect(screen.getAllByRole("button", { name: "Revoke" })).toHaveLength(4);
    expect(screen.getAllByRole("button", { name: "Grant" })).toHaveLength(2);
  });

  it("revokes one statement in place and reloads what the broker holds", async () => {
    const write = statement("writable", "write_files");
    vi.mocked(host.listConnectedFolders).mockResolvedValue([
      folder("writable", "Drafts"),
    ]);
    vi.mocked(host.listCapabilityConsents)
      .mockResolvedValueOnce([statement("writable", "read_files"), write])
      .mockResolvedValue([statement("writable", "read_files")]);
    const user = userEvent.setup();
    render(<FoldersView chat={chat} />);

    await screen.findByText("Write files");
    const revokes = screen.getAllByRole("button", { name: "Revoke" });
    expect(revokes).toHaveLength(2);
    await user.click(revokes[1]);
    // The confirmation says the folder stays connected before anything acts.
    await user.click(
      await screen.findByRole("button", { name: "Revoke", hidden: false }),
    );

    await waitFor(() =>
      expect(host.revokeCapabilityConsent).toHaveBeenCalledWith(write),
    );
    // The write statement is gone; what remains of it is a grant affordance.
    await waitFor(() =>
      expect(screen.getAllByRole("button", { name: "Revoke" })).toHaveLength(1),
    );
    // The folder itself was not disconnected.
    expect(host.disconnectFolder).not.toHaveBeenCalled();
    expect(screen.getByText("Drafts")).toBeInTheDocument();
  });

  it("offers to grant what a read-only folder is missing and reloads once granted", async () => {
    vi.mocked(host.listConnectedFolders).mockResolvedValue([
      folder("read-only", "Archive"),
    ]);
    vi.mocked(host.listCapabilityConsents)
      .mockResolvedValueOnce([statement("read-only", "read_files")])
      .mockResolvedValue([
        statement("read-only", "read_files"),
        statement("read-only", "write_files"),
      ]);
    vi.mocked(host.grantFolderCapability).mockResolvedValue(true);
    const user = userEvent.setup();
    render(<FoldersView chat={chat} />);

    // The missing rungs are visible before any refusal: write and commands
    // each get a grant affordance beside the statements the folder holds.
    await screen.findByText("Read only");
    expect(screen.getByText("Write files")).toBeInTheDocument();
    expect(screen.getByText("Run commands")).toBeInTheDocument();
    const grants = screen.getAllByRole("button", { name: "Grant" });
    expect(grants).toHaveLength(2);

    await user.click(grants[0]);
    expect(host.grantFolderCapability).toHaveBeenCalledWith(
      chat,
      "read-only",
      "write_files",
    );
    // Consent happened natively; the panel just follows the broker's answer.
    await waitFor(() =>
      expect(screen.getByText("Read and write")).toBeInTheDocument(),
    );
    expect(screen.getAllByRole("button", { name: "Grant" })).toHaveLength(1);
  });

  it("distinguishes an unavailable folder and lets it be forgotten for good", async () => {
    vi.mocked(host.listConnectedFolders)
      .mockResolvedValueOnce([
        folder("live", "Drafts"),
        folder("unplugged", "External archive", "unavailable"),
      ])
      .mockResolvedValue([folder("live", "Drafts")]);
    vi.mocked(host.listCapabilityConsents).mockResolvedValue([
      statement("live", "read_files"),
    ]);
    const user = userEvent.setup();
    render(<FoldersView chat={chat} />);

    // A set-aside folder no longer vanishes: it is its own group, marked and
    // explained, distinct from both connected and previously approved.
    expect(await screen.findByText("External archive")).toBeInTheDocument();
    expect(screen.getByText("Unavailable")).toBeInTheDocument();
    expect(
      screen.queryByText("Available on this Mac"),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Forget" }));
    // Forgetting is destructive across every chat, so it confirms first.
    await user.click(await screen.findByRole("button", { name: "Forget" }));
    await waitFor(() =>
      expect(host.forgetFolder).toHaveBeenCalledWith("unplugged"),
    );
    await waitFor(() =>
      expect(screen.queryByText("External archive")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("Drafts")).toBeInTheDocument();
  });

  it("changes nothing when native widening consent is canceled", async () => {
    vi.mocked(host.listConnectedFolders).mockResolvedValue([
      folder("read-only", "Archive"),
    ]);
    vi.mocked(host.listCapabilityConsents).mockResolvedValue([
      statement("read-only", "read_files"),
    ]);
    const user = userEvent.setup();
    render(<FoldersView chat={chat} />);

    await screen.findByText("Read only");
    await user.click(screen.getAllByRole("button", { name: "Grant" })[0]);

    expect(host.grantFolderCapability).toHaveBeenCalledOnce();
    expect(host.listConnectedFolders).toHaveBeenCalledOnce();
    expect(screen.getByText("Read only")).toBeInTheDocument();
  });

  it("offers approved folders that are not already connected", async () => {
    vi.mocked(host.listConnectedFolders)
      .mockResolvedValueOnce([folder("connected", "Current project")])
      .mockResolvedValueOnce([
        folder("connected", "Current project"),
        folder("available", "Research"),
      ]);
    vi.mocked(host.listApprovedFolders).mockResolvedValue([
      { rootId: "connected", displayName: "Current project", status: "connected" },
      { rootId: "available", displayName: "Research", status: "connected" },
    ]);
    vi.mocked(host.connectApprovedFolder).mockResolvedValue({
      rootId: "available",
      displayName: "Research",
      status: "connected",
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
      { rootId: "available", displayName: "Research", status: "connected" },
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
    vi.mocked(host.listConnectedFolders).mockImplementation((requestedChat) =>
      requestedChat.id === firstChat.id
        ? firstResponse.promise
        : secondResponse.promise,
    );

    const { rerender } = render(<FoldersView chat={firstChat} />);
    rerender(<FoldersView chat={secondChat} />);

    secondResponse.resolve([folder("chat-b-root", "Chat B folder")]);
    expect(await screen.findByText("Chat B folder")).toBeInTheDocument();

    firstResponse.resolve([folder("chat-a-root", "Chat A folder")]);
    await waitFor(() =>
      expect(screen.queryByText("Chat A folder")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("Chat B folder")).toBeInTheDocument();
  });
});
