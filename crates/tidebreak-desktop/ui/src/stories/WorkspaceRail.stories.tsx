import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";
import { WorkspaceRailDraft } from "./workspace-rail/WorkspaceRailDraft";

async function checkWorkspaceActions(canvasElement: HTMLElement) {
  const toolbar = within(
    within(canvasElement).getByRole("toolbar", { name: "Workspace actions" }),
  );
  await expect(toolbar.getByRole("button", { name: "Add repo" })).toBeVisible();
  await expect(
    toolbar.getByRole("button", { name: "New workspace" }),
  ).toBeVisible();
  return toolbar;
}

const meta = {
  title: "Code/Workspace list draft",
  component: WorkspaceRailDraft,
  args: {
    scenario: "repository",
    onAddRepo: fn(),
    onNewWorkspace: fn(),
  },
  parameters: { layout: "fullscreen" },
  play: async ({ canvasElement }) => {
    await checkWorkspaceActions(canvasElement);
    canvasElement.dataset.railReady = "true";
  },
  render: (args) => <WorkspaceRailDraft key={args.scenario} {...args} />,
} satisfies Meta<typeof WorkspaceRailDraft>;
export default meta;
type Story = StoryObj<typeof meta>;

export const ByRepository: Story = {
  play: async ({ canvasElement }) => {
    await checkWorkspaceActions(canvasElement);
    const canvas = within(canvasElement);
    const local = within(canvas.getByRole("region", { name: "Local" }));
    const slack = within(canvas.getByRole("region", { name: "Slack" }));
    await expect(
      local.getByRole("button", { name: "tidebreak, 2 workspaces" }),
    ).toBeVisible();
    await expect(
      slack.getByRole("button", { name: "tidebreak, 3 workspaces" }),
    ).toBeVisible();
    await userEvent.click(
      local.getByRole("button", { name: "model-gateway, 2 workspaces" }),
    );
    await expect(
      local.queryByRole("button", { name: /^Fix credential refresh retries/ }),
    ).not.toBeInTheDocument();
    await expect(
      slack.getByRole("button", { name: /^Finish Slack decision cards/ }),
    ).toBeVisible();
    await userEvent.click(
      local.getByRole("button", { name: "Local, 4 workspaces" }),
    );
    await userEvent.click(
      local.getByRole("button", { name: "Local, 4 workspaces" }),
    );
    await expect(
      local.getByRole("button", { name: "model-gateway, 2 workspaces" }),
    ).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(
      local.getByRole("button", { name: "model-gateway, 2 workspaces" }),
    );
    canvasElement.dataset.railReady = "true";
  },
};
export const ByStatus: Story = {
  args: { scenario: "status" },
  play: async ({ canvasElement, args }) => {
    const toolbar = await checkWorkspaceActions(canvasElement);
    await userEvent.click(toolbar.getByRole("button", { name: "Add repo" }));
    await expect(args.onAddRepo).toHaveBeenCalled();
    await userEvent.click(
      toolbar.getByRole("button", { name: "New workspace" }),
    );
    await expect(args.onNewWorkspace).toHaveBeenCalled();
    const canvas = within(canvasElement);
    for (const label of ["Local", "Slack"]) {
      const source = within(canvas.getByRole("region", { name: label }));
      await expect(
        source.getByRole("button", { name: "Needs you, 1 workspace" }),
      ).toBeVisible();
      await expect(
        source.getByRole("button", { name: "Running, 2 workspaces" }),
      ).toBeVisible();
    }
    canvasElement.dataset.railReady = "true";
  },
};
export const OneSource: Story = {
  args: { scenario: "single-source" },
  play: async ({ canvasElement }) => {
    await checkWorkspaceActions(canvasElement);
    const canvas = within(canvasElement);
    await expect(
      canvas.queryByRole("region", { name: "Slack" }),
    ).not.toBeInTheDocument();
    await expect(
      canvas.getByRole("button", { name: "tidebreak, 3 workspaces" }),
    ).toBeVisible();
    canvasElement.dataset.railReady = "true";
  },
};
export const Collapsed: Story = { args: { scenario: "collapsed" } };
export const NarrowLongNames: Story = { args: { scenario: "long-names" } };
export const Empty: Story = { args: { scenario: "empty" } };
export const Loading: Story = { args: { scenario: "loading" } };
export const LoadFailure: Story = { args: { scenario: "error" } };
