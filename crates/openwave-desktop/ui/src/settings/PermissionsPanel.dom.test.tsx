// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ConsentStatementSnapshot } from "../api";
import { PermissionsPanel } from "./PermissionsPanel";

const listCapabilityConsents = vi.hoisted(() =>
  vi.fn<() => Promise<ConsentStatementSnapshot[]>>(),
);
vi.mock("../host", () => ({ listCapabilityConsents }));

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

function api(overrides: Partial<Record<keyof ApiClient, unknown>> = {}) {
  return {
    listConsentStatements: vi.fn().mockResolvedValue([execStatement]),
    revokeStandingGrant: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

beforeEach(() => {
  listCapabilityConsents.mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("PermissionsPanel", () => {
  it("revokes a tool grant after confirmation and drops the row", async () => {
    const client = api();
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

  it("renders a broker capability grant beside tool grants, without a revoke control", async () => {
    listCapabilityConsents.mockResolvedValue([folderWriteStatement]);
    const client = api();
    render(<PermissionsPanel client={client} />);

    // Both halves of the read model land in the same chat group: the folder
    // by its safe display name with its capability verb and picker
    // provenance, and only the tool grant offers revocation here.
    await screen.findByText("Documents");
    screen.getByText("cargo …");
    screen.getByText("Write files");
    screen.getByText(/with the folder picker/);
    screen.getByText("Managed with its folder");
    expect(screen.getAllByRole("button", { name: "Revoke" })).toHaveLength(1);
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
});
