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
};

const REPLACED_REASON = "replaced by a new connect approval";

/** An older connect for the same person as `live`, revoked by the newer one. */
const replacedNameless: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000004",
  channel_kind: "slack",
  external_identity: "U-CASEY",
  workspace_identity: "T-ACME",
  workspace_name: "Acme Corp",
  created_at: "2026-07-01T10:00:00Z",
  revoked_at: "2026-08-19T09:00:00Z",
  revoked_reason: REPLACED_REASON,
};

const replacedOther: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000005",
  channel_kind: "slack",
  external_identity: "U-ROBIN",
  display_name: "Robin",
  workspace_identity: "T-ACME",
  workspace_name: "Acme Corp",
  created_at: "2026-06-01T10:00:00Z",
  revoked_at: "2026-08-10T09:00:00Z",
  revoked_reason: REPLACED_REASON,
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
    expect(screen.queryByText("Pending approval")).toBeNull();
  });

  it("lets a workspace start using GitHub App access before its first task", async () => {
    const client = {
      listCodeGrants: vi.fn(async () => [workspace]),
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
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(listCodeGrants).toHaveBeenCalledTimes(3);
    expect(revokeCodeGrant).toHaveBeenCalledExactlyOnceWith(workspace.id);
    expect(screen.queryByRole("button", { name: "Revoke" })).toBeNull();
  });

  it("collapses replaced-connection history behind a per-workspace disclosure", async () => {
    const client = {
      listCodeGrants: vi.fn(async () => [
        live,
        replacedNameless,
        replacedOther,
      ]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText("Casey");

    // Collapsed by default: no replaced row leaks into the list.
    expect(screen.queryByText(new RegExp(REPLACED_REASON))).toBeNull();
    expect(screen.queryByText("Robin")).toBeNull();

    const disclosure = screen.getByRole("button", {
      name: "2 replaced connections",
    });
    await userEvent.setup().click(disclosure);

    expect(screen.getAllByText(new RegExp(REPLACED_REASON))).toHaveLength(2);
    expect(screen.getByText("Robin")).toBeTruthy();
  });

  it("keeps a non-replacement revocation inline, never behind the disclosure", async () => {
    const client = {
      listCodeGrants: vi.fn(async () => [stolen]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText("Jordan");

    // The theft reason is readable without any click.
    expect(screen.getByText(/treated as stolen/)).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: /replaced connection/ }),
    ).toBeNull();
  });

  it("borrows the display name another grant stored for the same identity", async () => {
    const client = {
      listCodeGrants: vi.fn(async () => [live, replacedNameless]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);
    await screen.findByText("Casey");

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "1 replaced connection" }));

    // The nameless replaced grant is titled with the live grant's name, and
    // the member id stays visible as each row's metadata line.
    expect(screen.getAllByText("Casey")).toHaveLength(2);
    expect(screen.getAllByText("U-CASEY")).toHaveLength(2);
  });

  it("falls back to a generic member label, showing the raw id exactly once", async () => {
    const nameless: CodeGrantSnapshot = {
      ...live,
      id: "6b1f9a34-0000-4000-8000-000000000006",
      external_identity: "U072QAMTDCN",
      display_name: undefined,
    };
    const client = {
      listCodeGrants: vi.fn(async () => [nameless]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);

    await screen.findByText("Slack member");
    expect(screen.getAllByText("U072QAMTDCN")).toHaveLength(1);
  });

  it("drops the metadata line when it would repeat the title", async () => {
    const idAsName: CodeGrantSnapshot = {
      ...live,
      id: "6b1f9a34-0000-4000-8000-000000000007",
      external_identity: "U-SELF",
      display_name: "U-SELF",
    };
    const client = {
      listCodeGrants: vi.fn(async () => [idAsName]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);

    await screen.findByText("U-SELF");
    expect(screen.getAllByText("U-SELF")).toHaveLength(1);
  });

  it("names the workspace from any grant with a real team name", async () => {
    const namelessWorkspaceGrant: CodeGrantSnapshot = {
      ...live,
      id: "6b1f9a34-0000-4000-8000-000000000008",
      workspace_name: undefined,
    };
    const echoedIdentity: CodeGrantSnapshot = {
      ...workspace,
      id: "6b1f9a34-0000-4000-8000-00000000000a",
      workspace_name: "T-ACME",
    };
    const client = {
      listCodeGrants: vi.fn(async () => [
        namelessWorkspaceGrant,
        echoedIdentity,
      ]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={client} />);

    // The workspace grant echoed the identity as its name; the person grant
    // has none at all — the group still keeps the identity, not a fake name.
    await screen.findByText("Slack · T-ACME");
    expect(screen.getByText("Workspace T-ACME")).toBeTruthy();

    cleanup();
    const named = {
      listCodeGrants: vi.fn(async () => [
        { ...echoedIdentity, workspace_name: undefined },
        live,
      ]),
    } as unknown as ApiClient;
    render(<ChannelsPanel client={named} />);

    // Any grant in the group holding a real name names the header and rows.
    await screen.findByText("Slack · Acme Corp");
    expect(screen.getByText("Workspace Acme Corp")).toBeTruthy();
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

it("offers personal subscription settings only for live person grants", async () => {
  const open = vi.fn();
  const client = {
    listCodeGrants: async () => [live, stolen, workspace],
  } as unknown as ApiClient;
  render(<ChannelsPanel client={client} onOpenInferencePreferences={open} />);
  const buttons = await screen.findAllByRole("button", {
    name: "Subscription settings",
  });
  expect(buttons).toHaveLength(1);
  await userEvent.setup().click(buttons[0]);
  expect(open).toHaveBeenCalledExactlyOnceWith(live.id);
});
