// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, CodeGrantSnapshot } from "../api";
import { ChannelsPanel } from "./ChannelsPanel";

const live: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000001",
  channel_kind: "slack",
  external_identity: "U-CASEY",
  display_name: "Casey",
  workspace_identity: "T-ACME",
  workspace_name: "Acme Corp",
  created_at: "2026-08-20T10:00:00Z",
};

const stolen: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000003",
  channel_kind: "slack",
  external_identity: "U-JORDAN",
  display_name: "Jordan",
  workspace_identity: "T-NIGHT",
  workspace_name: "Nightside Labs",
  created_at: "2026-08-01T10:00:00Z",
  revoked_at: "2026-08-26T18:00:00Z",
  revoked_reason:
    "a rotated refresh token was replayed; the credential is treated as stolen",
};

const workspace: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000009",
  kind: "workspace",
  channel_kind: "slack",
  external_identity: "T-ACME",
  display_name: "tidebreak-slack",
  workspace_identity: "T-ACME",
  workspace_name: "Acme Corp",
  created_at: "2026-09-08T10:00:00Z",
  channels: [
    {
      channel_id: "C1",
      repository: "acme/tools",
      state: "confirmed",
      set_by_identity: "U1",
      set_by_display: "Casey",
    },
    {
      channel_id: "C1",
      repository: "acme/web",
      state: "pending",
      set_by_identity: "U1",
      set_by_display: "Casey",
    },
    {
      channel_id: "C1",
      repository: "acme/api",
      state: "pending",
      set_by_identity: "U1",
      set_by_display: "Casey",
    },
    {
      channel_id: "C1",
      repository: "acme/legacy",
      state: "superseded",
      set_by_identity: "U1",
      set_by_display: "Casey",
    },
    {
      channel_id: "C2",
      repository: "acme/private",
      state: "pending",
      set_by_identity: "U2",
      set_by_display: "Sam",
    },
  ],
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
    await user.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", {
        name: "Revoke",
      }),
    );
    await waitFor(() => expect(revokeCodeGrant).toHaveBeenCalledWith(live.id));

    await screen.findByText("Casey");
    await user.click(
      screen.getAllByRole("button", { name: "Revoke this workspace" })[0],
    );
    await user.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", {
        name: "Revoke workspace",
      }),
    );
    await waitFor(() =>
      expect(revokeCodeGrantWorkspace).toHaveBeenCalledWith("slack", "T-ACME"),
    );
  });

  it("uses GitHub App access for every channel without repository approval controls", async () => {
    const client = {
      listCodeGrants: vi.fn(async () => [workspace]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText("Workspace Acme Corp");

    expect(
      screen.getByText(
        /Every channel uses the repositories available to this instance’s GitHub App/,
      ),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Revoke" })).toBeTruthy();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(
      screen.queryByRole("button", {
        name: /Approve|Add channel|Add repositories/,
      }),
    ).toBeNull();
    // Old approval records do not define or display the instance's access.
    for (const entry of workspace.channels ?? []) {
      expect(screen.queryByText(entry.repository)).toBeNull();
      expect(screen.queryByText(entry.channel_id)).toBeNull();
    }
    expect(screen.queryByText("Pending approval")).toBeNull();
  });

  it("lets a workspace start using GitHub App access before its first task", async () => {
    const client = {
      listCodeGrants: vi.fn(async () => [{ ...workspace, channels: [] }]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText("Workspace Acme Corp");
    expect(screen.getByRole("button", { name: "Revoke" })).toBeTruthy();
    expect(screen.queryByRole("form")).toBeNull();
    expect(
      screen.queryByRole("button", { name: /Add channel|Approve/ }),
    ).toBeNull();
  });

  it("keeps a revoked workspace and its reason visible without offering actions", async () => {
    const client = {
      listCodeGrants: vi.fn().mockResolvedValue([
        {
          ...workspace,
          revoked_at: "2026-09-09T10:00:00Z",
          revoked_reason: "Administrator revoked access",
        },
      ]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText("Workspace Acme Corp");
    expect(screen.getByText(/Administrator revoked access/)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("retries only the grants read after a revoke succeeds but refresh fails", async () => {
    const listCodeGrants = vi
      .fn()
      .mockResolvedValueOnce([workspace])
      .mockRejectedValueOnce(new Error("Connection lost"))
      .mockResolvedValueOnce([
        { ...workspace, revoked_at: "2026-09-09T10:00:00Z" },
      ]);
    const revokeCodeGrant = vi.fn().mockResolvedValue(undefined);
    render(
      <ChannelsPanel
        client={{ listCodeGrants, revokeCodeGrant } as unknown as ApiClient}
      />,
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Revoke" }));
    await user.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", {
        name: "Revoke",
      }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Connection lost",
    );
    expect(screen.getByRole("button", { name: "Revoke" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Refresh grants" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(listCodeGrants).toHaveBeenCalledTimes(3);
    expect(revokeCodeGrant).toHaveBeenCalledExactlyOnceWith(workspace.id);
    expect(screen.queryByRole("button", { name: "Revoke" })).toBeNull();
  });

  it("says where connecting starts when nothing is connected", async () => {
    const client = {
      listCodeGrants: vi.fn(async () => []),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText("No channels connected");
  });

  it("leaves loading and lets the owner retry a failed grants read", async () => {
    const listCodeGrants = vi
      .fn()
      .mockRejectedValueOnce(new Error("The machine could not be reached."))
      .mockResolvedValueOnce([]);
    const client = { listCodeGrants } as unknown as ApiClient;

    const { container } = render(<ChannelsPanel client={client} />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The machine could not be reached.",
    );
    expect(screen.queryByText("Loading grants…")).toBeNull();
    expect(container.querySelector(".settings-panel")).toHaveAttribute(
      "aria-busy",
      "false",
    );

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText("No channels connected")).toBeTruthy();
    expect(listCodeGrants).toHaveBeenCalledTimes(2);
  });
});
