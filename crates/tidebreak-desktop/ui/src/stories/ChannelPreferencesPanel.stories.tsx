import { expect, userEvent, within } from "storybook/test";
import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ApiClient } from "@/api";
import { ChannelPreferencesPanel } from "@/settings/ChannelPreferencesPanel";
import type { ChannelPreferencesSnapshot } from "@/settings/channelPreferences";
import { harnessDoctor } from "./fixtures";
const preferences: ChannelPreferencesSnapshot = {
  harness: "claude_code",
  model: null,
  respond_automatically: true,
  instructions: "Keep replies brief and link to our runbooks.",
  channel_id: "C04ENGINEERING",
  workspace_identity: "T04ACME",
  settings_path: "/settings/channels",
  can_edit: true,
};
const client = {
  getChannelPreferences: async () => preferences,
  setChannelPreferences: async (
    _grant: string,
    _channel: string,
    value: object,
  ) => ({ ...preferences, ...value }),
  getHarnessDoctor: async () => harnessDoctor,
  listModels: async () => ({
    models: [
      {
        key: "model_gateway::example",
        display_name: "Example model",
        available: true,
        supports_tools: true,
      },
    ],
    roles: [],
  }),
  listCodeHarnessModels: async () => ({
    models: [{ id: "example-model", label: "Example model" }],
    source: "model_gateway",
  }),
} as unknown as ApiClient;
const meta = {
  title: "Settings/Channel preferences",
  component: ChannelPreferencesPanel,
  args: { client, grantId: "grant", channelId: "C04ENGINEERING" },
} satisfies Meta<typeof ChannelPreferencesPanel>;
export default meta;
type Story = StoryObj<typeof meta>;
export const Configured: Story = {};
export const Internal: Story = {
  args: {
    client: {
      ...client,
      getChannelPreferences: async () => ({
        ...preferences,
        harness: "internal",
        model: "model_gateway::example",
      }),
    } as unknown as ApiClient,
  },
};
export const Inherited: Story = {
  args: {
    client: {
      ...client,
      getChannelPreferences: async () => ({
        ...preferences,
        harness: null,
        model: null,
        instructions: "",
        respond_automatically: null,
      }),
    } as unknown as ApiClient,
  },
};
export const ReadOnly: Story = {
  args: {
    client: {
      ...client,
      getChannelPreferences: async () => ({ ...preferences, can_edit: false }),
    } as unknown as ApiClient,
  },
};
export const Loading: Story = {
  args: {
    client: {
      ...client,
      getChannelPreferences: () => new Promise(() => {}),
    } as unknown as ApiClient,
  },
};
export const Failed: Story = {
  args: {
    client: {
      ...client,
      getChannelPreferences: async () => {
        throw new Error("The machine could not be reached.");
      },
    } as unknown as ApiClient,
  },
};
export const LongInstructions: Story = {
  args: {
    client: {
      ...client,
      getChannelPreferences: async () => ({
        ...preferences,
        instructions:
          "Link to the relevant runbook before proposing a change. ".repeat(90),
      }),
    } as unknown as ApiClient,
  },
};

export const SaveFailed: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("switch", { name: "Respond automatically" }),
    );
    await expect(canvas.findByRole("alert")).resolves.toHaveTextContent(
      "could not be saved",
    );
  },
  args: {
    client: {
      ...client,
      setChannelPreferences: async () => {
        throw new Error("The channel settings could not be saved. Try again.");
      },
    } as unknown as ApiClient,
  },
};
