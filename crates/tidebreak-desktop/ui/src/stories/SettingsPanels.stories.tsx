import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";
import { AgentsPanel } from "@/settings/AgentsPanel";
import { AppearancePanel } from "@/settings/AppearancePanel";
import { CompactionPanel } from "@/settings/CompactionPanel";
import { ModelsPanel } from "@/settings/ModelsPanel";
import { NotificationsPanel } from "@/settings/NotificationsPanel";
import { UpdatesPanel } from "@/settings/UpdatesPanel";
import {
  setFinishedNotificationsEnabled,
  setNeedsYouNotificationsEnabled,
} from "@/NotificationPreferences";
import type { PromptCacheRetention } from "@/api";
import type { ThemeMode } from "@/theme";
import {
  DEFAULT_UPDATE_PREFERENCES,
  type DesktopUpdatePreferences,
  type DesktopUpdateState,
} from "@/updates";
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
  panel:
    | "appearance"
    | "notifications"
    | "agents"
    | "context"
    | "models"
    | "updates";
  loadState?: "ready" | "loading" | "failed";
  turnRecapsEnabled?: boolean;
  promptCacheRetention?: PromptCacheRetention;
  theme?: ThemeMode;
  updateState?: DesktopUpdateState;
  upToDate?: boolean;
  /** Notifications: tell you when an agent stops for you. */
  needsYou?: boolean;
  /** Notifications: tell you when an agent finishes or fails. */
  finished?: boolean;
  updatePreferences?: DesktopUpdatePreferences | null;
  /** Updates: why the automatic-download setting could not be saved. */
  updatePreferencesError?: string | null;
};

/**
 * The panel reads its switches from this device's storage when it mounts,
 * so the story writes them first.
 */
function NotificationsShowcase({
  needsYou,
  finished,
}: {
  needsYou: boolean;
  finished: boolean;
}) {
  useState(() => {
    setNeedsYouNotificationsEnabled(needsYou);
    setFinishedNotificationsEnabled(finished);
    return null;
  });
  return <NotificationsPanel />;
}

function SettingsShowcase({
  panel,
  loadState = "ready",
  turnRecapsEnabled = true,
  promptCacheRetention = "five_minutes",
  theme = "system",
  updateState = idleUpdate,
  upToDate = false,
  needsYou = true,
  finished = true,
  updatePreferences = DEFAULT_UPDATE_PREFERENCES,
  updatePreferencesError = null,
}: SettingsShowcaseProps) {
  if (panel === "appearance") {
    return <AppearancePanel mode={theme} onChange={fn()} />;
  }
  if (panel === "notifications") {
    return (
      <NotificationsShowcase
        key={`${needsYou}-${finished}`}
        needsYou={needsYou}
        finished={finished}
      />
    );
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
      appVersion="0.114.0"
      preferences={updatePreferences}
      preferencesError={updatePreferencesError}
      onCheck={fn(async () => updateState)}
      onDownload={fn(async () => updateState)}
      onRestart={fn(async () => {})}
      onAutomaticDownloadsChange={fn()}
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

/** Both switches on: what a new install sees. */
export const Notifications: Story = {
  args: { panel: "notifications" },
};

/** Told when an agent needs you, but not when one finishes. */
export const NotificationsNeedsYouOnly: Story = {
  args: { panel: "notifications", needsYou: true, finished: false },
};

export const NotificationsOff: Story = {
  args: { panel: "notifications", needsYou: false, finished: false },
};

export const NotificationsCompact: Story = {
  args: { panel: "notifications" },
  globals: { viewport: { value: "compact", isRotated: false } },
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

export const UpdateCheckFailed: Story = {
  args: {
    panel: "updates",
    updateState: {
      ...idleUpdate,
      error:
        "Could not check for updates. Tidebreak could not reach the update server. Check your internet connection and try again.",
    },
  },
};

export const UpdateAvailable: Story = {
  args: {
    panel: "updates",
    updateState: {
      status: "available",
      version: "0.115.0",
      error: null,
      enabled: true,
    },
    updatePreferences: { automaticDownloads: false, managed: false },
  },
};

export const UpdatesManagedByOrganization: Story = {
  args: {
    panel: "updates",
    updatePreferences: { automaticDownloads: false, managed: true },
  },
};

/** The organization keeps automatic downloads on. */
export const UpdatesManagedOn: Story = {
  args: {
    panel: "updates",
    updatePreferences: { automaticDownloads: true, managed: true },
  },
};

/** The download could not be saved, so the release stays on offer. */
export const UpdateAvailableDownloadFailed: Story = {
  args: {
    panel: "updates",
    updateState: {
      status: "available",
      version: "0.115.0",
      error:
        "Not enough disk space to download the update. Free up space, then try again.",
      enabled: true,
    },
  },
};

/** The desktop has not reported the setting yet. */
export const UpdatesPreferencesLoading: Story = {
  args: { panel: "updates", updatePreferences: null },
};

export const UpdatesPreferenceSaveFailed: Story = {
  args: {
    panel: "updates",
    updatePreferencesError: "Could not save the setting. Try again.",
  },
};
