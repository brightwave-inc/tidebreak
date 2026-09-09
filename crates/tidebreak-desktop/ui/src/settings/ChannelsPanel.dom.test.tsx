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

  it("shows workspace grants with their channels and a revoke control", async () => {
    const revokeCodeGrant = vi.fn(async () => ({
      ...workspace,
      revoked_at: "2026-09-08T11:00:00Z",
    }));
    const client = {
      listCodeGrants: vi.fn(async () => [workspace]),
      revokeCodeGrant,
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText("Workspace Acme Corp");
    const channel = screen.getByRole("region", { name: "Channel C1" });
    expect(within(channel).getByText("acme/tools")).toBeTruthy();
    expect(within(channel).getByText("Approved repositories")).toBeTruthy();
    expect(within(channel).getByText("Pending approval")).toBeTruthy();
    expect(within(channel).getByText("Previous repositories (1)")).toBeTruthy();
    expect(within(channel).queryByText("acme/private")).toBeNull();
    expect(screen.getByRole("button", { name: "Revoke" })).toBeTruthy();
  });

  it("approves every pending repository in one channel and reloads its scope", async () => {
    const approved = {
      ...workspace,
      channels: workspace.channels?.map((entry) =>
        entry.channel_id === "C1" && entry.state === "pending"
          ? { ...entry, state: "confirmed" }
          : entry,
      ),
    };
    const listCodeGrants = vi
      .fn()
      .mockResolvedValueOnce([workspace])
      .mockResolvedValueOnce([approved]);
    const approveWorkspaceGrantChannelRepositories = vi
      .fn()
      .mockResolvedValue(undefined);
    render(
      <ChannelsPanel
        client={
          {
            listCodeGrants,
            approveWorkspaceGrantChannelRepositories,
          } as unknown as ApiClient
        }
      />,
    );
    const channel = await screen.findByRole("region", { name: "Channel C1" });
    await userEvent
      .setup()
      .click(
        within(channel).getByRole("button", {
          name: "Approve all pending repositories",
        }),
      );
    await waitFor(() =>
      expect(approveWorkspaceGrantChannelRepositories).toHaveBeenCalledWith(
        workspace.id,
        "C1",
        ["acme/web", "acme/api"],
      ),
    );
    await waitFor(() =>
      expect(within(channel).queryByText("Pending approval")).toBeNull(),
    );
    expect(listCodeGrants).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("status")).toHaveTextContent(
      "Repositories approved for C1.",
    );
    expect(
      within(screen.getByRole("region", { name: "Channel C2" })).getByText(
        "Pending approval",
      ),
    ).toBeTruthy();
  });

  it("caps a large pending approval at the first 100 repositories", async () => {
    const channels = Array.from({ length: 101 }, (_, index) => ({
      channel_id: "C1",
      repository: `acme/repository-${index}`,
      state: "pending",
      set_by_identity: "U1",
      set_by_display: "Casey",
    }));
    const approveWorkspaceGrantChannelRepositories = vi
      .fn()
      .mockResolvedValue(undefined);
    const listCodeGrants = vi
      .fn()
      .mockResolvedValue([{ ...workspace, channels }]);
    render(
      <ChannelsPanel
        client={
          {
            listCodeGrants,
            approveWorkspaceGrantChannelRepositories,
          } as unknown as ApiClient
        }
      />,
    );
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Approve first 100" }));
    await waitFor(() =>
      expect(approveWorkspaceGrantChannelRepositories).toHaveBeenCalledWith(
        workspace.id,
        "C1",
        channels.slice(0, 100).map((entry) => entry.repository),
      ),
    );
    expect(approveWorkspaceGrantChannelRepositories).toHaveBeenCalledTimes(1);
    expect(listCodeGrants).toHaveBeenCalledTimes(2);
  });

  it("adds a channel before its first task with trimmed explicit names and URLs", async () => {
    const listCodeGrants = vi
      .fn()
      .mockResolvedValue([{ ...workspace, channels: [] }]);
    const approveWorkspaceGrantChannelRepositories = vi
      .fn()
      .mockResolvedValue(undefined);
    render(
      <ChannelsPanel
        client={
          {
            listCodeGrants,
            approveWorkspaceGrantChannelRepositories,
          } as unknown as ApiClient
        }
      />,
    );
    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "Add channel" }),
    );
    const form = screen.getByRole("form", { name: "Add channel repositories" });
    await user.type(
      within(form).getByRole("textbox", { name: /Slack channel ID/ }),
      "  CNEW123  ",
    );
    await user.type(
      within(form).getByRole("textbox", { name: /Repositories/ }),
      " acme/tools , https://github.com/acme/web\nacme/tools ",
    );
    await user.click(
      within(form).getByRole("button", { name: "Approve repositories" }),
    );
    await waitFor(() =>
      expect(approveWorkspaceGrantChannelRepositories).toHaveBeenCalledWith(
        workspace.id,
        "CNEW123",
        ["acme/tools", "https://github.com/acme/web"],
      ),
    );
    await waitFor(() => expect(screen.queryByRole("form")).toBeNull());
    expect(listCodeGrants).toHaveBeenCalledTimes(2);
  });

  it("keeps the editor after an authorization failure and does not report success", async () => {
    const listCodeGrants = vi.fn().mockResolvedValue([workspace]);
    const approveWorkspaceGrantChannelRepositories = vi
      .fn()
      .mockRejectedValue(new Error("403: administrator access required"));
    render(
      <ChannelsPanel
        client={
          {
            listCodeGrants,
            approveWorkspaceGrantChannelRepositories,
          } as unknown as ApiClient
        }
      />,
    );
    const user = userEvent.setup();
    const channel = await screen.findByRole("region", { name: "Channel C1" });
    await user.click(
      within(channel).getByRole("button", { name: "Add repositories" }),
    );
    const editor = within(channel).getByRole("textbox", {
      name: /Repositories/,
    });
    await user.type(editor, "acme/extra");
    await user.click(
      within(channel).getByRole("button", { name: "Approve repositories" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "403: administrator access required",
    );
    expect(editor).toHaveValue("acme/extra");
    expect(listCodeGrants).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("status")).toBeNull();
    expect(
      within(channel).getByRole("button", { name: "Approve repositories" }),
    ).toBeEnabled();
  });

  it("reports saved approvals separately from a failed refresh and retries only the read", async () => {
    const listCodeGrants = vi
      .fn()
      .mockResolvedValueOnce([workspace])
      .mockRejectedValueOnce(new Error("Connection lost"))
      .mockResolvedValueOnce([workspace]);
    const approveWorkspaceGrantChannelRepositories = vi
      .fn()
      .mockResolvedValue(undefined);
    render(
      <ChannelsPanel
        client={
          {
            listCodeGrants,
            approveWorkspaceGrantChannelRepositories,
          } as unknown as ApiClient
        }
      />,
    );
    const user = userEvent.setup();
    const channel = await screen.findByRole("region", { name: "Channel C1" });
    await user.click(
      within(channel).getByRole("button", { name: "Add repositories" }),
    );
    await user.type(
      within(channel).getByRole("textbox", { name: /Repositories/ }),
      "acme/extra",
    );
    await user.click(
      within(channel).getByRole("button", { name: "Approve repositories" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Repositories were approved, but the list could not refresh. Error: Connection lost",
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Repositories approved for C1.",
    );
    expect(screen.queryByRole("form")).toBeNull();
    expect(
      within(channel).getByRole("button", { name: "Add repositories" }),
    ).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Refresh grants" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(listCodeGrants).toHaveBeenCalledTimes(3);
    expect(approveWorkspaceGrantChannelRepositories).toHaveBeenCalledTimes(1);
    expect(
      within(channel).getByRole("button", { name: "Add repositories" }),
    ).toBeEnabled();
  });

  it("keeps revoked workspace scopes visible without offering approval controls", async () => {
    const client = {
      listCodeGrants: vi
        .fn()
        .mockResolvedValue([
          {
            ...workspace,
            revoked_at: "2026-09-09T10:00:00Z",
            revoked_reason: "Administrator revoked access",
          },
        ]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByRole("region", { name: "Channel C1" });
    expect(screen.getByText("acme/tools")).toBeTruthy();
    expect(
      screen.queryByRole("button", {
        name: /Approve|Add channel|Add repositories|Revoke/,
      }),
    ).toBeNull();
  });

  it("rejects missing channel IDs, wildcard scopes, and more than 100 repositories", async () => {
    const approveWorkspaceGrantChannelRepositories = vi.fn();
    render(
      <ChannelsPanel
        client={
          {
            listCodeGrants: vi
              .fn()
              .mockResolvedValue([{ ...workspace, channels: [] }]),
            approveWorkspaceGrantChannelRepositories,
          } as unknown as ApiClient
        }
      />,
    );
    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "Add channel" }),
    );
    const channelId = screen.getByRole("textbox", { name: /Slack channel ID/ });
    const repositories = screen.getByRole("textbox", { name: /Repositories/ });
    const approve = screen.getByRole("button", {
      name: "Approve repositories",
    });
    await user.click(approve);
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Enter the Slack channel ID.",
    );
    await user.type(channelId, "CNEW");
    await user.click(approve);
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Enter between 1 and 100 explicit repositories.",
    );
    await user.type(repositories, "acme/*");
    await user.click(approve);
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Wildcards are not supported.",
    );
    await user.clear(repositories);
    await user.click(repositories);
    await user.paste(
      Array.from(
        { length: 101 },
        (_, index) => `acme/repository-${index}`,
      ).join("\n"),
    );
    await user.click(approve);
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Enter between 1 and 100 explicit repositories.",
    );
    expect(approveWorkspaceGrantChannelRepositories).not.toHaveBeenCalled();
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
