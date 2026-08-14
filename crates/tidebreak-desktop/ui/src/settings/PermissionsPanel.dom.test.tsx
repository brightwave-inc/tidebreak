// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ConsentStatementSnapshot } from "../api";
import type { ConnectedFolder } from "../host";
import {
  levelLabel,
  PermissionsPanel,
  statementsForChat,
} from "./PermissionsPanel";

const listCapabilityConsents = vi.hoisted(() =>
  vi.fn<() => Promise<ConsentStatementSnapshot[]>>(),
);
const revokeCapabilityConsent = vi.hoisted(() =>
  vi.fn<() => Promise<boolean>>(),
);
const listConnectedFolders = vi.hoisted(() =>
  vi.fn<() => Promise<ConnectedFolder[]>>(),
);
const grantFolderCapability = vi.hoisted(() =>
  vi.fn<() => Promise<boolean | null>>(),
);
vi.mock("../host", () => ({
  listCapabilityConsents,
  revokeCapabilityConsent,
  listConnectedFolders,
  grantFolderCapability,
}));

const execStatement: ConsentStatementSnapshot = {
  handle: {
    kind: "tool_grant",
    call_id: "11111111-1111-1111-1111-111111111111",
  },
  level: { level: "chat", chat_id: "22222222-2222-2222-2222-222222222222" },
  level_title: "Quarterly filings",
  verb: { kind: "tool", action: "exec", approval: "exec_may_run_networked_command" },
  resource: {
    kind: "action_scope",
    scope: { scope: "any_args_for", command: "cargo" },
  },
  method: "approval_card",
  granted_at: "2026-07-29T12:00:00Z",
};

const folderWriteStatement: ConsentStatementSnapshot = {
  handle: {
    kind: "capability_grant",
    grant_id: "44444444-4444-4444-4444-444444444444",
  },
  level: { level: "chat", chat_id: "22222222-2222-2222-2222-222222222222" },
  level_title: "Quarterly filings",
  verb: { kind: "capability", capability: "write_files" },
  resource: {
    kind: "host_root",
    root_id: "55555555-5555-5555-5555-555555555555",
    display_name: "Documents",
  },
  method: "folder_picker",
  granted_at: "2026-07-30T12:00:00Z",
};

const folderReadStatement: ConsentStatementSnapshot = {
  ...folderWriteStatement,
  handle: {
    kind: "capability_grant",
    grant_id: "66666666-6666-6666-6666-666666666666",
  },
  verb: { kind: "capability", capability: "read_files" },
  method: "permission_dialog",
};

function api(overrides: Partial<Record<keyof ApiClient, unknown>> = {}) {
  return {
    listConsentStatements: vi.fn().mockResolvedValue([execStatement]),
    revokeStandingGrant: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

beforeEach(() => {
  listCapabilityConsents.mockResolvedValue([]);
  listConnectedFolders.mockResolvedValue([]);
  grantFolderCapability.mockResolvedValue(true);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("PermissionsPanel", () => {
  it("revokes a tool grant after confirmation and drops the row", async () => {
    const listConsentStatements = vi.fn().mockResolvedValue([execStatement]);
    const client = api({
      listConsentStatements,
      revokeStandingGrant: vi.fn().mockImplementation(() => {
        // The reload after a revocation reads back what the server now holds.
        listConsentStatements.mockResolvedValue([]);
        return Promise.resolve(undefined);
      }),
    });
    render(<PermissionsPanel client={client} />);

    // The statement renders under its chat, worded as the width of the consent.
    await screen.findByText("Quarterly filings");
    screen.getByText("cargo …");

    await userEvent.click(screen.getByRole("button", { name: "Revoke" }));
    // The confirmation names what will start asking again before it acts.
    await userEvent.click(
      await screen.findByRole("button", { name: "Revoke", hidden: false }),
    );

    await waitFor(() =>
      expect(client.revokeStandingGrant).toHaveBeenCalledWith(
        "11111111-1111-1111-1111-111111111111",
      ),
    );
    await waitFor(() =>
      expect(screen.queryByText("cargo …")).not.toBeInTheDocument(),
    );
  });

  it("revokes a broker capability grant through the host and reloads the list", async () => {
    listCapabilityConsents.mockResolvedValue([folderWriteStatement]);
    revokeCapabilityConsent.mockImplementation(() => {
      // The reloaded list is what the broker now holds.
      listCapabilityConsents.mockResolvedValue([]);
      return Promise.resolve(true);
    });
    const client = api();
    render(<PermissionsPanel client={client} />);

    // Both halves of the read model land in the same chat group, and both
    // offer the same revocation control.
    await screen.findByText("Documents");
    screen.getByText("cargo …");
    screen.getByText("Write files");
    screen.getByText(/with the folder picker/);
    const revokes = screen.getAllByRole("button", { name: "Revoke" });
    expect(revokes).toHaveLength(2);

    // The capability row is listed after the tool grant in its group.
    await userEvent.click(revokes[1]);
    await userEvent.click(
      await screen.findByRole("button", { name: "Revoke", hidden: false }),
    );
    await waitFor(() =>
      expect(revokeCapabilityConsent).toHaveBeenCalledWith(
        folderWriteStatement,
      ),
    );
    await waitFor(() =>
      expect(screen.queryByText("Documents")).not.toBeInTheDocument(),
    );
    expect(client.revokeStandingGrant).not.toHaveBeenCalled();
  });

  it("offers read access back on an attached folder that allows nothing", async () => {
    // The state a reader can reach and could not leave: read revoked, so the
    // folder is attached and unusable, and nothing re-grants it on its own.
    const chatId = "22222222-2222-2222-2222-222222222222";
    listCapabilityConsents.mockResolvedValue([]);
    listConnectedFolders.mockResolvedValue([
      {
        rootId: "55555555-5555-5555-5555-555555555555",
        displayName: "Documents",
        status: "connected",
      },
    ]);
    grantFolderCapability.mockImplementation(() => {
      listCapabilityConsents.mockResolvedValue([folderReadStatement]);
      listConnectedFolders.mockResolvedValue([]);
      return Promise.resolve(true);
    });
    const client = api({ listConsentStatements: vi.fn().mockResolvedValue([]) });
    render(
      <PermissionsPanel client={client} chat={{ id: chatId, project_id: null }} />,
    );

    await screen.findByText("Documents");
    await userEvent.click(screen.getByRole("button", { name: "Grant" }));

    await waitFor(() =>
      expect(grantFolderCapability).toHaveBeenCalledWith(
        { id: chatId },
        "55555555-5555-5555-5555-555555555555",
        "read_files",
      ),
    );
    // The restored grant is listed as an ordinary revocable statement, and the
    // offer to restore it is gone.
    await screen.findByText("Read files");
    expect(screen.queryByRole("button", { name: "Grant" })).toBeNull();
  });

  it("names a project statement as reaching past the chat that made it", async () => {
    const projectStatement: ConsentStatementSnapshot = {
      ...execStatement,
      level: {
        level: "project",
        project_id: "33333333-3333-3333-3333-333333333333",
      },
      level_title: "Filings",
    };
    const client = api({
      listConsentStatements: vi.fn().mockResolvedValue([projectStatement]),
    });
    render(<PermissionsPanel client={client} />);

    // Not "Filings" alone: the reader has to be able to tell a statement that
    // covers conversations they have not started yet.
    await screen.findByText("Everything in Filings");
  });

  it("says so when nothing is saved", async () => {
    const client = api({
      listConsentStatements: vi.fn().mockResolvedValue([]),
    });
    render(<PermissionsPanel client={client} />);
    await screen.findByText(/Nothing saved yet/);
    expect(client.listConsentStatements).toHaveBeenCalled();
  });

  it("ignores a previous chat load that resolves after the current chat", async () => {
    const oldChatId = "22222222-2222-2222-2222-222222222222";
    const newChatId = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    const newStatement: ConsentStatementSnapshot = {
      ...execStatement,
      handle: {
        kind: "tool_grant",
        call_id: "99999999-9999-9999-9999-999999999999",
      },
      level: { level: "chat", chat_id: newChatId },
      level_title: "Current chat",
      resource: {
        kind: "action_scope",
        scope: { scope: "any_args_for", command: "pnpm" },
      },
    };
    const oldLoad = deferred<ConsentStatementSnapshot[]>();
    const newLoad = deferred<ConsentStatementSnapshot[]>();
    const client = api({
      listConsentStatements: vi
        .fn()
        .mockReturnValueOnce(oldLoad.promise)
        .mockReturnValueOnce(newLoad.promise),
    });
    const { rerender } = render(
      <PermissionsPanel
        client={client}
        chat={{ id: oldChatId, project_id: null }}
      />,
    );

    rerender(
      <PermissionsPanel
        client={client}
        chat={{ id: newChatId, project_id: null }}
      />,
    );
    await act(async () => newLoad.resolve([newStatement]));
    expect(await screen.findByText("pnpm …")).toBeInTheDocument();

    await act(async () => oldLoad.resolve([execStatement]));
    expect(screen.getByText("pnpm …")).toBeInTheDocument();
    expect(screen.queryByText("cargo …")).not.toBeInTheDocument();
  });

  it("does not reload a previous chat after its revocation finishes", async () => {
    const oldChatId = "22222222-2222-2222-2222-222222222222";
    const newChatId = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    const currentStatement: ConsentStatementSnapshot = {
      ...execStatement,
      handle: {
        kind: "tool_grant",
        call_id: "99999999-9999-9999-9999-999999999999",
      },
      level: { level: "chat", chat_id: newChatId },
      level_title: "Current chat",
      resource: {
        kind: "action_scope",
        scope: { scope: "any_args_for", command: "pnpm" },
      },
    };
    const revokeDone = deferred<void>();
    const listConsentStatements = vi
      .fn()
      .mockResolvedValueOnce([execStatement])
      .mockResolvedValue([currentStatement]);
    const client = api({
      listConsentStatements,
      revokeStandingGrant: vi.fn(() => revokeDone.promise),
    });
    const { rerender } = render(
      <PermissionsPanel
        client={client}
        chat={{ id: oldChatId, project_id: null }}
      />,
    );

    await screen.findByText("cargo …");
    await userEvent.click(screen.getByRole("button", { name: "Revoke" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "Revoke", hidden: false }),
    );
    await waitFor(() =>
      expect(client.revokeStandingGrant).toHaveBeenCalledTimes(1),
    );

    rerender(
      <PermissionsPanel
        client={client}
        chat={{ id: newChatId, project_id: null }}
      />,
    );
    expect(await screen.findByText("pnpm …")).toBeInTheDocument();

    await act(async () => revokeDone.resolve());
    expect(listConsentStatements).toHaveBeenCalledTimes(2);
    expect(screen.getByText("pnpm …")).toBeInTheDocument();
    expect(screen.queryByText("cargo …")).not.toBeInTheDocument();
  });
});


describe("permissions labeling and chat filter", () => {
  it("names a missing chat subject as deleted", () => {
    expect(
      levelLabel(execStatement, { chatIds: new Set() }),
    ).toBe("Deleted chat 222222…2222");
    // shortOpaqueId keeps first 6 and last 4 for long ids.
    expect(levelLabel(execStatement, { chatIds: new Set() })).toBe(
      "Deleted chat 222222…2222",
    );
  });

  it("keeps only statements that reach one chat", () => {
    const other = {
      ...execStatement,
      handle: { kind: "tool_grant" as const, call_id: "99999999-9999-9999-9999-999999999999" },
      level: {
        level: "chat" as const,
        chat_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
      },
    };
    const project = {
      ...folderWriteStatement,
      handle: {
        kind: "capability_grant" as const,
        grant_id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
      },
      level: {
        level: "project" as const,
        project_id: "cccccccc-cccc-cccc-cccc-cccccccccccc",
      },
      level_title: "Filings",
    };
    const filtered = statementsForChat(
      [execStatement, other, project],
      { id: execStatement.level.level === "chat" ? execStatement.level.chat_id : "", project_id: "cccccccc-cccc-cccc-cccc-cccccccccccc" },
    );
    expect(filtered.map((s) => s.handle)).toEqual([
      execStatement.handle,
      project.handle,
    ]);
  });
});
