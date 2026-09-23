import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import type { ComponentProps } from "react";
import { McpDirectoryList } from "@/settings/McpDirectory";
import { mcpDirectoryServer, mcpDirectoryServers } from "./fixtures";

const linear = mcpDirectoryServers.find((server) => server.id === "linear");
if (linear === undefined) throw new Error("the directory fixture lists Linear");

/** The directory in the settings column, the way the MCP servers section
 * shows it above the import and the configured servers. */
function DirectoryShowcase(props: ComponentProps<typeof McpDirectoryList>) {
  return (
    <div className="settings-panel-content mx-auto max-w-[62rem]">
      <McpDirectoryList {...props} />
    </div>
  );
}

const meta = {
  title: "Settings/MCP directory",
  component: DirectoryShowcase,
  parameters: { layout: "padded" },
  args: {
    servers: mcpDirectoryServers,
    configured: [],
    adding: null,
    disabled: false,
    loadError: null,
    addError: null,
    onAdd: fn(),
  },
} satisfies Meta<typeof DirectoryShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Every server, with what it does, how it signs in, and where it connects.
 * No entry is on the tested list, so none shows a tier. */
export const Directory: Story = {};

/** A search that matches nothing points at adding a server by hand. */
export const SearchWithNoResults: Story = {
  args: { initialQuery: "salesforce" },
};

/** One click in: the row shows the add running, and every other Add waits
 * for it. */
export const AddInProgress: Story = {
  args: { adding: "linear", disabled: true },
};

/** A server configured at the same address reads as added. */
export const Added: Story = {
  args: { configured: [mcpDirectoryServer(linear)] },
};

/** An add the server refused says why beside the list. */
export const AddFailed: Story = {
  args: {
    addError:
      "Could not add Stripe: This server asks you to sign in, but Tidebreak cannot complete its sign-in. Its sign-in service does not let new apps register (no dynamic client registration), and Tidebreak has no client ID for it.",
  },
};

export const Loading: Story = {
  args: { servers: null },
};

export const LoadFailed: Story = {
  args: { loadError: "Could not reach Tidebreak's server." },
};
