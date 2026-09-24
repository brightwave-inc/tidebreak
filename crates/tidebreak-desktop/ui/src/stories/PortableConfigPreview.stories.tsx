import type { Meta, StoryObj } from "@storybook/react-vite";
import { userEvent, within } from "storybook/test";

import type {
  WorkspaceConfigDocument,
  WorkspaceConfigPreviewEntry,
} from "@/api/types";
import { PortableConfigSection } from "@/settings/PortableConfigSection";

const document: WorkspaceConfigDocument = {
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
        setup_script: "pnpm install --frozen-lockfile",
        quick_actions: [
          {
            name: "Start dev server",
            command: "scripts/dev.sh",
            auto_run_on_create: true,
          },
        ],
      },
    ],
    mcp_servers: [
      {
        name: "docs",
        command: "/opt/mcp/docs",
        args: ["--stdio", "--root", "Team Docs"],
        env: ["DOCS_TOKEN"],
        env_from: ["PATH"],
        cwd: "/Users/alex/src/docs",
        request_timeout_ms: 60_000,
        enabled: true,
      },
      {
        name: "search",
        args: [],
        env: [],
        env_from: [],
        url: "https://mcp.example.com/search",
        bearer_token_env: "SEARCH_TOKEN",
        request_timeout_ms: 60_000,
        enabled: true,
      },
      {
        name: "status",
        args: [],
        env: [],
        env_from: [],
        url: "https://status.example.com/mcp",
        request_timeout_ms: 60_000,
        enabled: true,
      },
      {
        name: "tickets",
        args: [],
        env: [],
        env_from: [],
        url: "https://mcp.example.com/tickets",
        bearer_token_stored: true,
        headers: ["X-Api-Key"],
        request_timeout_ms: 60_000,
        enabled: true,
      },
    ],
  },
};

const repoKey = "https://github.com/brightwave-inc/tidebreak.git";

function previewClient(entries: WorkspaceConfigPreviewEntry[]) {
  return {
    exportWorkspaceConfig: async () => document,
    previewWorkspaceConfig: async () => ({ entries }),
    applyWorkspaceConfig: async () => ({ applied: 0, skipped: entries.length }),
  };
}

function entry(
  section: WorkspaceConfigPreviewEntry["section"],
  key: string,
  status: WorkspaceConfigPreviewEntry["status"],
  fields: Partial<
    Pick<WorkspaceConfigPreviewEntry, "differing_fields" | "remap_fields">
  > = {},
): WorkspaceConfigPreviewEntry {
  return {
    section,
    key,
    status,
    differing_fields: fields.differing_fields ?? [],
    remap_fields: fields.remap_fields ?? [],
  };
}

/** Import the file the way a reader does, then wait for the preview. */
async function openPreview(canvasElement: HTMLElement) {
  const file = new File([JSON.stringify(document)], "tidebreak-config.json", {
    type: "application/json",
  });
  await userEvent.upload(
    within(canvasElement).getByLabelText("Import workspace configuration"),
    file,
  );
  return within(canvasElement.ownerDocument.body).findByRole("dialog");
}

/**
 * The import preview. Every row shows what the entry runs and connects to,
 * and only new entries start on Add. An MCP server that runs a local command
 * or sends a credential from this computer imports turned off unless the
 * reader starts it; the desktop also confirms a local command natively.
 */
const meta = {
  title: "Settings/Portable configuration preview",
  component: PortableConfigSection,
  parameters: { layout: "fullscreen" },
  play: async ({ canvasElement }) => {
    await openPreview(canvasElement);
  },
} satisfies Meta<typeof PortableConfigSection>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A new local command server, a new remote server that sends a credential,
 * a new remote server that sends nothing, and a repository that already
 * matches. The first two wait turned off; the third imports as the file has
 * it. */
export const Clean: Story = {
  args: {
    client: previewClient([
      entry("mcp_servers", "docs", "new"),
      entry("mcp_servers", "search", "new"),
      entry("mcp_servers", "status", "new"),
      entry("code_repositories", repoKey, "identical"),
    ]),
  },
};

/** The reader chose to start the local server. Apply then shows the
 * desktop's native dialog listing the command before anything runs. */
export const StartLocalServer: Story = {
  args: {
    client: previewClient([entry("mcp_servers", "docs", "new")]),
  },
  play: async ({ canvasElement }) => {
    const dialog = within(await openPreview(canvasElement));
    await userEvent.click(
      dialog.getByRole("switch", { name: "Start docs after import" }),
    );
  },
};

/** The reader chose to start the remote server that sends SEARCH_TOKEN. It
 * connects when the import is applied; the switch is the consent. */
export const StartCredentialServer: Story = {
  args: {
    client: previewClient([entry("mcp_servers", "search", "new")]),
  },
  play: async ({ canvasElement }) => {
    const dialog = within(await openPreview(canvasElement));
    await userEvent.click(
      dialog.getByRole("switch", { name: "Start search after import" }),
    );
  },
};

/** A remote server whose bearer token and header value were stored on the
 * computer that exported it. The values never travel in the file, so the
 * row says what to enter here, and the server imports turned off. */
export const StoredCredentialServer: Story = {
  args: {
    client: previewClient([entry("mcp_servers", "tickets", "new")]),
  },
  play: async ({ canvasElement }) => {
    await openPreview(canvasElement);
  },
};

/** A conflict starts on Skip and offers only Skip or Replace. */
export const Conflicts: Story = {
  args: {
    client: previewClient([
      entry("mcp_servers", "docs", "conflict", {
        differing_fields: ["command", "args"],
      }),
    ]),
  },
};

/** Entries that need a path or command on this machine start on Skip. */
export const RemapsNeeded: Story = {
  args: {
    client: previewClient([
      entry("code_repositories", repoKey, "needs_remap", {
        remap_fields: ["root_path"],
      }),
      entry("mcp_servers", "docs", "needs_remap", {
        remap_fields: ["command", "cwd"],
      }),
    ]),
  },
};

/** The desktop's native dialog was declined, so nothing was imported. The
 * preview stays open so the reader can import the server turned off. */
export const StartDeclined: Story = {
  args: {
    client: {
      ...previewClient([entry("mcp_servers", "docs", "new")]),
      applyWorkspaceConfig: async () => {
        throw "You did not allow the local MCP commands, so nothing was imported.";
      },
    },
  },
  play: async ({ canvasElement }) => {
    const dialog = within(await openPreview(canvasElement));
    await userEvent.click(
      dialog.getByRole("switch", { name: "Start docs after import" }),
    );
    await userEvent.click(dialog.getByRole("button", { name: "Apply" }));
    await dialog.findByText(/did not allow the local MCP commands/);
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
  play: async ({ canvasElement }) => {
    const file = new File(["{}"], "tidebreak-config.json", {
      type: "application/json",
    });
    await userEvent.upload(
      within(canvasElement).getByLabelText("Import workspace configuration"),
      file,
    );
    await within(canvasElement).findByRole("alert");
  },
};

/** The path is filled in, so the repository moves from Skip to Add. */
export const PathFilledIn: Story = {
  args: {
    client: previewClient([
      entry("code_repositories", repoKey, "needs_remap", {
        remap_fields: ["root_path"],
      }),
    ]),
  },
  play: async ({ canvasElement }) => {
    const dialog = within(await openPreview(canvasElement));
    await userEvent.type(
      dialog.getByLabelText(`Remap root_path for ${repoKey}`),
      "/Users/alex/code/tidebreak",
    );
  },
};

/** The import ran; the section says what it applied and skipped. */
export const Imported: Story = {
  args: {
    client: {
      ...previewClient([
        entry("mcp_servers", "status", "new"),
        entry("mcp_servers", "docs", "conflict", {
          differing_fields: ["args"],
        }),
        entry("code_repositories", repoKey, "identical"),
      ]),
      applyWorkspaceConfig: async () => ({ applied: 1, skipped: 2 }),
    },
  },
  play: async ({ canvasElement }) => {
    const dialog = within(await openPreview(canvasElement));
    await userEvent.click(dialog.getByRole("button", { name: "Apply" }));
    await within(canvasElement).findByText("Import finished");
  },
};

/** A server would not start, so the import undid itself and says so. */
export const ImportFailed: Story = {
  args: {
    client: {
      ...previewClient([
        entry("mcp_servers", "status", "new"),
        entry("code_repositories", repoKey, "identical"),
      ]),
      applyWorkspaceConfig: async () => {
        throw new Error(
          "external MCP server status failed to start: the server answered 503. Nothing changed.",
        );
      },
    },
  },
  play: async ({ canvasElement }) => {
    const dialog = within(await openPreview(canvasElement));
    await userEvent.click(dialog.getByRole("button", { name: "Apply" }));
    await dialog.findByText("The import failed");
  },
};
