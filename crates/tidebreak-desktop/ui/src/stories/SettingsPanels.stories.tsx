import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import { AgentsPanel } from "@/settings/AgentsPanel";
import { AppearancePanel } from "@/settings/AppearancePanel";
import { CompactionPanel } from "@/settings/CompactionPanel";
import { ModelsPanel } from "@/settings/ModelsPanel";
import { UpdatesPanel } from "@/settings/UpdatesPanel";
import type { PromptCacheRetention } from "@/api";
import type { ThemeMode } from "@/theme";
import type { DesktopUpdateState } from "@/updates";
import {
  SettingsStoryHarness,
  storyModels,
  storySettings,
} from "./SettingsStoryHarness";

const idleUpdate: DesktopUpdateState = {
  status: "idle",
  version: null,
  error: null,
  enabled: true,
};

type SettingsShowcaseProps = {
  panel: "appearance" | "agents" | "context" | "models" | "updates";
  loadState?: "ready" | "loading" | "failed";
  turnRecapsEnabled?: boolean;
  promptCacheRetention?: PromptCacheRetention;
  theme?: ThemeMode;
  updateState?: DesktopUpdateState;
  upToDate?: boolean;
};

function SettingsShowcase({
  panel,
  loadState = "ready",
  turnRecapsEnabled = true,
  promptCacheRetention = "five_minutes",
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
        settings={
          turnRecapsEnabled
            ? storySettings
            : { ...storySettings, code_turn_recaps_enabled: false }
        }
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
  if (panel === "models") {
    return (
      <SettingsStoryHarness
        state={loadState === "ready" ? "configured" : loadState}
        settings={{
          ...storySettings,
          prompt_cache_retention: promptCacheRetention,
        }}
      >
        {(client) => <ModelsPanel client={client} models={storyModels} />}
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

export const AgentsRecapsOff: Story = {
  args: { panel: "agents", turnRecapsEnabled: false },
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
};

export const ContextFailure: Story = {
  args: { panel: "context", loadState: "failed" },
};

export const ModelsDefaultRetention: Story = {
  args: { panel: "models" },
};

export const ModelsOneHourRetention: Story = {
  args: { panel: "models", promptCacheRetention: "one_hour" },
};

export const ModelsLoading: Story = {
  args: { panel: "models", loadState: "loading" },
};

export const ModelsFailure: Story = {
  args: { panel: "models", loadState: "failed" },
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
