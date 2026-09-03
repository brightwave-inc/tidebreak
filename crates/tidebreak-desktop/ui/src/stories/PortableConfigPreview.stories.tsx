import type { Meta, StoryObj } from "@storybook/react-vite";

import type { WorkspaceConfigPreviewEntry } from "@/api/types";
import { PortableConfigSection } from "@/settings/PortableConfigSection";

const document = {
  tidebreak_config: 1,
  exported_at: "2026-09-02T12:00:00Z",
  sections: {
    code_repositories: [
      {
        display_name: "tidebreak",
        origin_url: "https://github.com/brightwave-inc/tidebreak.git",
        root_path: "/Users/alex/src/tidebreak",
        default_base_ref: "main",
        branch_prefix: "tidebreak/",
        quick_actions: [],
      },
    ],
    mcp_servers: [
      {
        name: "docs",
        command: "/opt/mcp/docs",
        args: [],
        env: ["TOKEN"],
        env_from: [],
        request_timeout_ms: 60_000,
        enabled: true,
      },
    ],
  },
};

function previewClient(entries: WorkspaceConfigPreviewEntry[]) {
  return {
    exportWorkspaceConfig: async () => document,
    previewWorkspaceConfig: async () => ({ entries }),
    applyWorkspaceConfig: async () => ({ applied: 0, skipped: entries.length }),
  };
}

const meta = {
  title: "Settings/Portable configuration preview",
  component: PortableConfigSection,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof PortableConfigSection>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Clean: Story = {
  args: {
    client: previewClient([
      {
        section: "mcp_servers",
        key: "docs",
        status: "new",
        differing_fields: [],
        remap_fields: [],
      },
      {
        section: "code_repositories",
        key: "https://github.com/brightwave-inc/tidebreak.git",
        status: "identical",
        differing_fields: [],
        remap_fields: [],
      },
    ]),
  },
};

export const Conflicts: Story = {
  args: {
    client: previewClient([
      {
        section: "mcp_servers",
        key: "docs",
        status: "conflict",
        differing_fields: ["command", "args"],
        remap_fields: [],
      },
    ]),
  },
};

export const RemapsNeeded: Story = {
  args: {
    client: previewClient([
      {
        section: "code_repositories",
        key: "https://github.com/brightwave-inc/tidebreak.git",
        status: "needs_remap",
        differing_fields: [],
        remap_fields: ["root_path"],
      },
      {
        section: "mcp_servers",
        key: "docs",
        status: "needs_remap",
        differing_fields: [],
        remap_fields: ["command"],
      },
    ]),
  },
};

export const UnsupportedVersion: Story = {
  args: {
    client: {
      exportWorkspaceConfig: async () => document,
      previewWorkspaceConfig: async () => {
        throw new Error(
          "this file uses format version 99, which this Tidebreak does not read; upgrade Tidebreak or export again from a matching version",
        );
      },
      applyWorkspaceConfig: async () => ({ applied: 0, skipped: 0 }),
    },
  },
};
