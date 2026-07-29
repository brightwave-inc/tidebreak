// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, StandingGrantSnapshot } from "../api";
import { PermissionsPanel } from "./PermissionsPanel";

const execGrant: StandingGrantSnapshot = {
  source_call_id: "11111111-1111-1111-1111-111111111111",
  level: { level: "chat", chat_id: "22222222-2222-2222-2222-222222222222" },
  level_title: "Quarterly filings",
  action: "exec",
  approval: "exec_may_run_networked_command",
  scope: { scope: "any_args_for", command: "cargo" },
  granted_at: "2026-07-29T12:00:00Z",
};

function api(overrides: Partial<Record<keyof ApiClient, unknown>> = {}) {
  return {
    listStandingGrants: vi.fn().mockResolvedValue([execGrant]),
    revokeStandingGrant: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("PermissionsPanel", () => {
  it("revokes a grant after confirmation and drops the row", async () => {
    const client = api();
    render(<PermissionsPanel client={client} />);

    // The grant renders under its chat, worded as the width of the consent.
    await screen.findByText("Quarterly filings");
    screen.getByText("cargo …");

    await userEvent.click(screen.getByRole("button", { name: "Revoke" }));
    // The confirmation names what will start asking again before it acts.
    await userEvent.click(
      await screen.findByRole("button", { name: "Revoke", hidden: false }),
    );

    await waitFor(() =>
      expect(client.revokeStandingGrant).toHaveBeenCalledWith(
        execGrant.source_call_id,
      ),
    );
    await waitFor(() =>
      expect(screen.queryByText("cargo …")).not.toBeInTheDocument(),
    );
  });

  it("names a project grant as reaching past the chat that made it", async () => {
    const projectGrant: StandingGrantSnapshot = {
      ...execGrant,
      level: { level: "project", project_id: "33333333-3333-3333-3333-333333333333" },
      level_title: "Filings",
    };
    const client = api({
      listStandingGrants: vi.fn().mockResolvedValue([projectGrant]),
    });
    render(<PermissionsPanel client={client} />);

    // Not "Filings" alone: the reader has to be able to tell a grant that
    // covers conversations they have not started yet.
    await screen.findByText("Everything in Filings");
  });

  it("says so when nothing is saved", async () => {
    const client = api({ listStandingGrants: vi.fn().mockResolvedValue([]) });
    render(<PermissionsPanel client={client} />);
    await screen.findByText(/Nothing saved yet/);
    expect(client.listStandingGrants).toHaveBeenCalled();
  });
});
