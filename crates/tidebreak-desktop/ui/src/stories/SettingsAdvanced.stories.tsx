import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import { CodingHarnessesPanel } from "@/settings/CodingHarnessesPanel";
import { ConnectedAppsPanel } from "@/settings/ConnectedAppsPanel";
import { ExecPanel } from "@/settings/ExecPanel";
import { ModelsPanel } from "@/settings/ModelsPanel";
import { PermissionsPanel } from "@/settings/PermissionsPanel";
import { ProvidersPanel } from "@/settings/ProvidersPanel";
import { VoiceTranscriptionPanel } from "@/settings/VoiceTranscriptionPanel";
import {
  SettingsStoryHarness,
  type SettingsStoryState,
  storyModels,
  storyProviders,
} from "./SettingsStoryHarness";

type SettingsAdvancedShowcaseProps = {
  panel:
    | "providers"
    | "models"
    | "voice"
    | "exec"
    | "harnesses"
    | "connected-apps"
    | "permissions";
  state?: SettingsStoryState;
  managed?: boolean;
};

function SettingsAdvancedShowcase({
  panel,
  state = "configured",
  managed = false,
}: SettingsAdvancedShowcaseProps) {
  return (
    <SettingsStoryHarness state={state}>
      {(client) => {
        switch (panel) {
          case "providers":
            return (
              <ProvidersPanel
                providers={storyProviders}
                models={storyModels}
                client={client}
                managed={managed}
                onChanged={fn()}
                expandProvider={managed ? undefined : "openai"}
              />
            );
          case "models":
            return (
              <ModelsPanel
                client={client}
                models={storyModels}
                managed={managed}
                onChanged={fn()}
              />
            );
          case "voice":
            return <VoiceTranscriptionPanel client={client} />;
          case "exec":
            return <ExecPanel client={client} />;
          case "harnesses":
            return <CodingHarnessesPanel client={client} />;
          case "connected-apps":
            return <ConnectedAppsPanel client={client} managed={managed} />;
          case "permissions":
            return (
              <PermissionsPanel
                client={client}
                knownChatIds={new Set(["chat-filings"])}
                knownProjectIds={new Set(["project-launch"])}
              />
            );
        }
      }}
    </SettingsStoryHarness>
  );
}

const meta = {
  title: "Settings/Advanced panels",
  component: SettingsAdvancedShowcase,
  parameters: { layout: "fullscreen" },
  args: { panel: "providers", state: "configured", managed: false },
} satisfies Meta<typeof SettingsAdvancedShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ProvidersConfigured: Story = {};

export const ProvidersManaged: Story = {
  args: { managed: true, state: "managed" },
};

export const ModelsConfigured: Story = {
  args: { panel: "models" },
};

export const ModelsLoading: Story = {
  args: { panel: "models", state: "loading" },
};

export const ModelsFailure: Story = {
  args: { panel: "models", state: "failed" },
};

export const ModelsManagedCompact: Story = {
  args: { panel: "models", managed: true, state: "managed" },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const VoiceConfigured: Story = {
  args: { panel: "voice" },
};

export const VoiceFailure: Story = {
  args: { panel: "voice", state: "failed" },
};

export const VoiceUnavailableCompact: Story = {
  args: { panel: "voice", state: "disabled" },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const CodeExecutionConfigured: Story = {
  args: { panel: "exec" },
};

export const CodeExecutionFailure: Story = {
  args: { panel: "exec", state: "failed" },
};

export const CodeExecutionDockerRefused: Story = {
  args: { panel: "exec", state: "docker-refused" },
};

export const CodeExecutionUnavailableCompact: Story = {
  args: { panel: "exec", state: "disabled" },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const CodingHarnessesConfigured: Story = {
  args: { panel: "harnesses" },
};

export const CodingHarnessesLoading: Story = {
  args: { panel: "harnesses", state: "loading" },
};

export const CodingHarnessesFailureCompact: Story = {
  args: { panel: "harnesses", state: "failed" },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const ConnectedAppsConfigured: Story = {
  args: { panel: "connected-apps" },
};

export const ConnectedAppsLoading: Story = {
  args: { panel: "connected-apps", state: "loading" },
};

export const ConnectedAppsEmpty: Story = {
  args: { panel: "connected-apps", state: "empty" },
};

export const ConnectedAppsMcpImportSummary: Story = {
  args: { panel: "connected-apps", state: "empty" },
};

export const ConnectedAppsMcpImportSummaryCompact: Story = {
  args: { panel: "connected-apps", state: "empty" },
  parameters: { viewport: { defaultViewport: "compact" } },
};

export const ConnectedAppsFailure: Story = {
  args: { panel: "connected-apps", state: "failed" },
};

export const ConnectedAppsManagedCompact: Story = {
  args: {
    panel: "connected-apps",
    managed: true,
    state: "managed",
  },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const ConnectedAppsLoopbackHttpConsent: Story = {
  args: { panel: "connected-apps", state: "empty" },
};

export const PermissionsConfigured: Story = {
  args: { panel: "permissions" },
};

export const PermissionsLoading: Story = {
  args: { panel: "permissions", state: "loading" },
};

export const PermissionsEmpty: Story = {
  args: { panel: "permissions", state: "empty" },
};

export const PermissionsFailureCompact: Story = {
  args: { panel: "permissions", state: "failed" },
  globals: { viewport: { value: "compact", isRotated: false } },
};
