import type { Meta, StoryObj } from "@storybook/react-vite";
import type { McpOAuthStatus } from "@/generated/wire";
import { McpHealthChip, McpOAuthControl } from "@/settings/McpPanel";
import { SettingsSection } from "@/settings/primitives";
import {
  mcpOauthAccessDenied,
  mcpOauthAuthorizing,
  mcpOauthConnected,
  mcpOauthExpired,
  mcpOauthNotConnected,
  mcpOauthUnsupported,
} from "./fixtures";

const states: Array<{ label: string; status: McpOAuthStatus }> = [
  { label: "Not connected", status: mcpOauthNotConnected },
  { label: "Authorizing", status: mcpOauthAuthorizing },
  { label: "Connected", status: mcpOauthConnected },
  { label: "Expired", status: mcpOauthExpired },
  { label: "Access denied", status: mcpOauthAccessDenied },
  { label: "Unsupported", status: mcpOauthUnsupported },
];

function OauthStatesShowcase() {
  return (
    <div className="mx-auto flex max-w-xl flex-col gap-6 p-6">
      {states.map((row) => (
        <SettingsSection key={row.status.state} title={row.label}>
          <div className="flex flex-wrap items-center gap-3">
            <McpHealthChip health="healthy" />
            <McpOAuthControl status={row.status} />
          </div>
        </SettingsSection>
      ))}
    </div>
  );
}

const meta = {
  title: "Settings/MCP OAuth",
  component: OauthStatesShowcase,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof OauthStatesShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The OAuth control beside health, across the states that matter. */
export const States: Story = {};
