import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, within } from "storybook/test";

import type { ApiClient, CodeGrantSnapshot } from "@/api";
import { ChannelsPanel } from "@/settings/ChannelsPanel";

/**
 * A client that answers the panel's one read and refuses to write. A story
 * that could revoke would behave differently the second time it is opened.
 */
function stubClient(grants: CodeGrantSnapshot[]): ApiClient {
  return {
    listCodeGrants: async () => grants,
  } as unknown as ApiClient;
}

const live: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000001",
  channel_kind: "slack",
  external_identity: "U04CASEY",
  display_name: "Casey Nakamura",
  workspace_identity: "T04ACME",
  workspace_name: "Acme Corp",
  created_at: "2026-08-20T10:00:00Z",
};

const rotated: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000002",
  channel_kind: "slack",
  external_identity: "U04SAM",
  display_name: "Sam Okafor",
  workspace_identity: "T04ACME",
  workspace_name: "Acme Corp",
  created_at: "2026-08-12T10:00:00Z",
  rotated_at: "2026-08-27T09:30:00Z",
};

const stolen: CodeGrantSnapshot = {
  id: "6b1f9a34-0000-4000-8000-000000000003",
  channel_kind: "slack",
  external_identity: "U08JORDAN",
  display_name: "Jordan Reyes",
  workspace_identity: "T08NIGHT",
  workspace_name: "Nightside Labs",
  created_at: "2026-08-01T10:00:00Z",
  revoked_at: "2026-08-26T18:00:00Z",
  revoked_reason:
    "a rotated refresh token was replayed; the credential is treated as stolen",
};

/**
 * The grants an external channel holds on this machine. The states that
 * matter: workspaces with live grants and their per-row and whole-workspace
 * revokes, a theft-revoked grant whose reason must stay visible, and the
 * empty install that says where connecting actually starts.
 */
const meta = {
  title: "Settings/Channels",
  component: ChannelsPanel,
  args: { client: stubClient([live, rotated, stolen]) },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ChannelsPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Two workspaces: one healthy, one whose only grant was revoked for theft. */
export const ConnectedWorkspaces: Story = {};

/** Nothing connected. The empty state points at the channel, not a button. */
export const NothingConnected: Story = {
  args: { client: stubClient([]) },
};

/** The first read is still in flight. */
export const Loading: Story = {
  args: {
    client: {
      listCodeGrants: () => new Promise<CodeGrantSnapshot[]>(() => {}),
    } as unknown as ApiClient,
  },
};

/** The machine could not answer the grants read. */
export const LoadFailed: Story = {
  args: {
    client: {
      listCodeGrants: async () => {
        throw new Error("The machine could not be reached.");
      },
    } as unknown as ApiClient,
  },
};

/** Only the theft-revoked grant: the reason is the notification of record. */
export const RevokedForTokenReuse: Story = {
  args: { client: stubClient([stolen]) },
};

const workspace: CodeGrantSnapshot = {
  ...live,
  id: "6b1f9a34-0000-4000-8000-000000000009",
  kind: "workspace",
  external_identity: "T04ACME",
  display_name: "tidebreak-slack",
  channels: [
    {
      channel_id: "C04ENGINEERING",
      repository: "acme/service",
      state: "confirmed",
      set_by_identity: "U04CASEY",
      set_by_display: "Casey Nakamura",
    },
    {
      channel_id: "C04ENGINEERING",
      repository: "acme/web",
      state: "pending",
      set_by_identity: "U04SAM",
      set_by_display: "Sam Okafor",
    },
    {
      channel_id: "C04ENGINEERING",
      repository: "acme/infrastructure",
      state: "pending",
      set_by_identity: "U04SAM",
      set_by_display: "Sam Okafor",
    },
    {
      channel_id: "C04ENGINEERING",
      repository: "acme/legacy",
      state: "superseded",
      set_by_identity: "U04CASEY",
      set_by_display: "Casey Nakamura",
    },
    {
      channel_id: "C04DESIGN",
      repository: "acme/design-system",
      state: "confirmed",
      set_by_identity: "U04CASEY",
      set_by_display: "Casey Nakamura",
    },
  ],
};

export const WorkspaceGitHubAccess: Story = {
  args: { client: stubClient([workspace, live]) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByText("Workspace Acme Corp")).toBeVisible();
    await expect(
      canvas.getByText(
        /Every channel uses the repositories available to this instance’s GitHub App/,
      ),
    ).toBeVisible();
    await expect(canvas.queryByRole("textbox")).not.toBeInTheDocument();
    await expect(
      canvas.queryByText("Pending approval"),
    ).not.toBeInTheDocument();
  },
};

export const BeforeFirstTask: Story = {
  args: { client: stubClient([{ ...workspace, channels: [] }]) },
};

export const LongWorkspaceName: Story = {
  args: {
    client: stubClient([
      {
        ...workspace,
        workspace_name:
          "Acme platform engineering and shared infrastructure development",
      },
    ]),
  },
};
