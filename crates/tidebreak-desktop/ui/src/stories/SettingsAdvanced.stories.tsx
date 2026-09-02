import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

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
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText("Loading model settings…"),
    ).toBeVisible();
  },
};

export const ModelsFailure: Story = {
  args: { panel: "models", state: "failed" },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText(
        "Error: Settings could not be loaded.",
      ),
    ).toBeVisible();
  },
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
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText(
        "Error: Settings could not be loaded.",
      ),
    ).toBeVisible();
  },
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
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText(
        "Error: Settings could not be loaded.",
      ),
    ).toBeVisible();
  },
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
  play: async ({ canvasElement }) => {
    const heading = await within(canvasElement).findByRole("heading", {
      name: "Coding harnesses",
    });
    await expect(heading.closest("[aria-busy]")).toHaveAttribute(
      "aria-busy",
      "true",
    );
  },
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
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText("Loading connected apps…"),
    ).toBeVisible();
  },
};

export const ConnectedAppsEmpty: Story = {
  args: { panel: "connected-apps", state: "empty" },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText(
        "No apps connected. Add a REST API here, or configure MCP servers in the editor below.",
      ),
    ).toBeVisible();
  },
};

const MCP_IMPORT_FILE_NAME =
  "desktop-mcp-configuration-for-the-customer-support-workspace.json";

const connectedAppsMcpImportPlay = async (canvasElement: HTMLElement) => {
  const canvas = within(canvasElement);
  const file = new File(
    [
      JSON.stringify({
        mcpServers: {
          calendar_sync_for_incident_team: {
            command: "npx",
            args: ["-y", "@example/calendar-mcp"],
            env: {
              CALENDAR_TOKEN_FOR_THE_CUSTOMER_SUPPORT_INCIDENT_WORKSPACE:
                "not-imported",
            },
          },
          "invalid.server.name.that.must.wrap": {
            command: "invalid-server",
          },
        },
      }),
    ],
    MCP_IMPORT_FILE_NAME,
    { type: "application/json" },
  );
  await userEvent.upload(
    await canvas.findByLabelText("Import MCP configuration"),
    file,
  );
  const summary = await canvas.findByRole("status");
  await expect(summary).toHaveTextContent(
    `1 server added to the editor from ${MCP_IMPORT_FILE_NAME}. 1 entry was skipped.`,
  );
  summary.scrollIntoView({ block: "center" });
};

export const ConnectedAppsMcpImportSummary: Story = {
  args: { panel: "connected-apps", state: "empty" },
  play: ({ canvasElement }) => connectedAppsMcpImportPlay(canvasElement),
};

export const ConnectedAppsMcpImportSummaryCompact: Story = {
  args: { panel: "connected-apps", state: "empty" },
  parameters: { viewport: { defaultViewport: "compact" } },
  play: ({ canvasElement }) => connectedAppsMcpImportPlay(canvasElement),
};

export const ConnectedAppsFailure: Story = {
  args: { panel: "connected-apps", state: "failed" },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findAllByText(
        "Settings could not be loaded.",
      ),
    ).not.toHaveLength(0);
  },
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
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: /Add REST API/ }),
    );
    await userEvent.type(canvas.getByLabelText(/^Name$/), "Local service");
    await userEvent.type(
      canvas.getByLabelText(/Base URL/),
      "http://127.0.0.1:23373/v0/mcp",
    );
    await expect(
      canvas.getByText(
        /This service runs on this computer without TLS. Tidebreak sends the credential in clear text to 127.0.0.1 only./,
      ),
    ).toBeVisible();
    await expect(canvas.getByRole("button", { name: /^Save$/ })).toBeDisabled();
    await expect(
      canvas.getByText(/Settings → MCP servers as a remote HTTP server/),
    ).toBeVisible();
  },
};

export const PermissionsConfigured: Story = {
  args: { panel: "permissions" },
};

export const PermissionsLoading: Story = {
  args: { panel: "permissions", state: "loading" },
  play: async ({ canvasElement }) => {
    const heading = await within(canvasElement).findByRole("heading", {
      name: "Permissions",
    });
    await expect(heading.closest("[aria-busy]")).toHaveAttribute(
      "aria-busy",
      "true",
    );
  },
};

export const PermissionsEmpty: Story = {
  args: { panel: "permissions", state: "empty" },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText(
        "Nothing saved yet. When you answer an approval with “always allow” or connect a folder, it appears here.",
      ),
    ).toBeVisible();
  },
};

export const PermissionsFailureCompact: Story = {
  args: { panel: "permissions", state: "failed" },
  globals: { viewport: { value: "compact", isRotated: false } },
};
