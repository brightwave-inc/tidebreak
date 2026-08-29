// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, CodeGrantSnapshot } from "../api";
import { ChannelsPanel } from "./ChannelsPanel";

const live: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000001",
  channel_kind: "slack",
  external_identity: "Casey",
  workspace_identity: "T-ACME",
  created_at: "2026-08-20T10:00:00Z",
};

const stolen: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000003",
  channel_kind: "slack",
  external_identity: "Jordan",
  workspace_identity: "T-NIGHT",
  created_at: "2026-08-01T10:00:00Z",
  revoked_at: "2026-08-26T18:00:00Z",
  revoked_reason:
    "a rotated refresh token was replayed; the credential is treated as stolen",
};

afterEach(cleanup);

describe("ChannelsPanel", () => {
  it("groups grants by workspace, keeps a theft revoke visible, and wires both revokes", async () => {
    const listCodeGrants = vi.fn(async () => [live, stolen]);
    const revokeCodeGrant = vi.fn(async () => ({
      ...live,
      revoked_at: "2026-08-28T10:00:00Z",
    }));
    const revokeCodeGrantWorkspace = vi.fn(async () => [
      { ...live, revoked_at: "2026-08-28T10:00:00Z" },
    ]);
    const client = {
      listCodeGrants,
      revokeCodeGrant,
      revokeCodeGrantWorkspace,
    } as unknown as ApiClient;

    render(<ChannelsPanel client={client} />);
    await screen.findByText("Casey");

    // The theft revoke and its reason are the notification of record.
    expect(
      screen.getByText(/treated as stolen/, { exact: false }),
    ).toBeTruthy();
    // A revoked grant offers no second revoke.
    expect(screen.getAllByRole("button", { name: "Revoke" })).toHaveLength(1);

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Revoke" }));
    await waitFor(() => expect(revokeCodeGrant).toHaveBeenCalledWith(live.id));

    await screen.findByText("Casey");
    await user.click(
      screen.getAllByRole("button", { name: "Revoke this workspace" })[0],
    );
    await waitFor(() =>
      expect(revokeCodeGrantWorkspace).toHaveBeenCalledWith("slack", "T-ACME"),
    );
  });

  it("says where connecting starts when nothing is connected", async () => {
    const client = {
      listCodeGrants: vi.fn(async () => []),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText(/No channel is connected/);
  });
});
