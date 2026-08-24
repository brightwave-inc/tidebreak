import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn, userEvent, within } from "storybook/test";

import { AgentsPanel } from "@/settings/AgentsPanel";
import { AppearancePanel } from "@/settings/AppearancePanel";
import { CompactionPanel } from "@/settings/CompactionPanel";
import { UpdatesPanel } from "@/settings/UpdatesPanel";
import type { ThemeMode } from "@/theme";
import type { DesktopUpdateState } from "@/updates";
import { SettingsStoryHarness } from "./SettingsStoryHarness";

const idleUpdate: DesktopUpdateState = {
  status: "idle",
  version: null,
  error: null,
  enabled: true,
};

type SettingsShowcaseProps = {
  panel: "appearance" | "agents" | "context" | "updates";
  loadState?: "ready" | "loading" | "failed";
  theme?: ThemeMode;
  updateState?: DesktopUpdateState;
  upToDate?: boolean;
};

function SettingsShowcase({
  panel,
  loadState = "ready",
  theme = "system",
  updateState = idleUpdate,
  upToDate = false,
}: SettingsShowcaseProps) {
  if (panel === "appearance") {
    return <AppearancePanel mode={theme} onChange={fn()} />;
  }
  if (panel === "agents") {
    return (
      <SettingsStoryHarness
        state={loadState === "ready" ? "configured" : loadState}
      >
        {(client) => <AgentsPanel client={client} />}
      </SettingsStoryHarness>
    );
  }
  if (panel === "context") {
    return (
      <SettingsStoryHarness
        state={loadState === "ready" ? "configured" : loadState}
      >
        {(client) => <CompactionPanel client={client} />}
      </SettingsStoryHarness>
    );
  }
  return (
    <UpdatesPanel
      state={updateState}
      upToDate={upToDate}
      onCheck={fn(async () => updateState)}
      onRestart={fn(async () => {})}
    />
  );
}

const meta = {
  title: "Settings/Core panels",
  component: SettingsShowcase,
  parameters: { layout: "fullscreen" },
  args: { panel: "appearance" },
} satisfies Meta<typeof SettingsShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

export const AppearanceSystem: Story = {};

export const AppearanceDark: Story = {
  args: { theme: "dark" },
  globals: { theme: "dark" },
};

export const Agents: Story = {
  args: { panel: "agents" },
};

export const AgentsLoading: Story = {
  args: { panel: "agents", loadState: "loading" },
};

export const AgentsFailure: Story = {
  args: { panel: "agents", loadState: "failed" },
};

export const ContextDefaults: Story = {
  args: { panel: "context" },
};

export const ContextAdvanced: Story = {
  args: { panel: "context" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Advanced" }),
    );
  },
};

export const ContextFailure: Story = {
  args: { panel: "context", loadState: "failed" },
};

export const UpdateReady: Story = {
  args: {
    panel: "updates",
    updateState: {
      status: "ready",
      version: "0.59.0",
      error: null,
      enabled: true,
    },
  },
};

export const UpdateDownloading: Story = {
  args: {
    panel: "updates",
    updateState: {
      status: "downloading",
      version: "0.59.0",
      error: null,
      enabled: true,
    },
  },
};

export const UpdateFailure: Story = {
  args: {
    panel: "updates",
    updateState: {
      ...idleUpdate,
      error: "The update signature could not be verified.",
    },
  },
};

export const UpdatesDisabled: Story = {
  args: {
    panel: "updates",
    updateState: { ...idleUpdate, enabled: false },
  },
};

export const UpToDate: Story = {
  args: { panel: "updates", upToDate: true },
};
