// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  WorkspaceConfigDocument,
  WorkspaceConfigApplyRequest,
  WorkspaceConfigPreview,
} from "@/api/types";
import { PortableConfigSection } from "./PortableConfigSection";

const document: WorkspaceConfigDocument = {
  tidebreak_config: 1,
  exported_at: "2026-09-02T00:00:00Z",
  sections: {
    code_repositories: [
      {
        display_name: "tidebreak",
        origin_url: "https://github.com/brightwave-inc/tidebreak.git",
        root_path: "/Users/alex/src/tidebreak",
        default_base_ref: "main",
        branch_prefix: "tidebreak/",
        setup_script: "pnpm install",
        quick_actions: [],
      },
    ],
    mcp_servers: [
      {
        name: "docs",
        command: "/opt/mcp/docs",
        args: ["--stdio", "--root", "My Docs"],
        env: ["TOKEN"],
        env_from: ["PATH"],
        cwd: "/srv/docs",
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
    ],
  },
};

afterEach(cleanup);

function renderSection(preview: WorkspaceConfigPreview) {
  const previewWorkspaceConfig = vi.fn(async () => preview);
  const applyWorkspaceConfig = vi.fn(
    async (_request: WorkspaceConfigApplyRequest) => ({
      applied: 1,
      skipped: 0,
    }),
  );
  render(
    <PortableConfigSection
      client={{
        exportWorkspaceConfig: async () => document,
        previewWorkspaceConfig,
        applyWorkspaceConfig,
      }}
    />,
  );
  return { applyWorkspaceConfig };
}

async function importDocument(user: ReturnType<typeof userEvent.setup>) {
  const file = new File([JSON.stringify(document)], "tidebreak-config.json", {
    type: "application/json",
  });
  await user.upload(
    screen.getByLabelText("Import workspace configuration"),
    file,
  );
}

function row(name: string): HTMLElement {
  const item = screen
    .getAllByRole("listitem")
    .find((candidate) => within(candidate).queryByText(name) !== null);
  if (!item) throw new Error(`no preview row for ${name}`);
  return item;
}

describe("PortableConfigSection", () => {
  it("shows preview statuses before applying", async () => {
    const user = userEvent.setup();
    const { applyWorkspaceConfig } = renderSection({
      entries: [
        {
          section: "mcp_servers",
          key: "docs",
          status: "conflict",
          differing_fields: ["command"],
          remap_fields: ["command"],
        },
      ],
    });
    await importDocument(user);

    expect(
      await screen.findByText("Conflicts with an existing record."),
    ).toBeVisible();
    expect(screen.getByText(/Differing: command/)).toBeVisible();
    expect(screen.getByLabelText("Remap command for docs")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Replace" }));
    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(applyWorkspaceConfig).toHaveBeenCalled());
    expect(applyWorkspaceConfig.mock.calls[0][0].decisions[0].action).toBe(
      "replace",
    );
  });

  it("shows what every entry runs and connects to", async () => {
    const user = userEvent.setup();
    renderSection({
      entries: [
        {
          section: "code_repositories",
          key: "https://github.com/brightwave-inc/tidebreak.git",
          status: "new",
          differing_fields: [],
          remap_fields: [],
        },
        {
          section: "mcp_servers",
          key: "docs",
          status: "new",
          differing_fields: [],
          remap_fields: [],
        },
        {
          section: "mcp_servers",
          key: "search",
          status: "new",
          differing_fields: [],
          remap_fields: [],
        },
      ],
    });
    await importDocument(user);
    await screen.findByLabelText("Import preview");

    const docs = within(row("docs"));
    expect(
      docs.getByText('/opt/mcp/docs --stdio --root "My Docs"'),
    ).toBeVisible();
    expect(docs.getByText("/srv/docs")).toBeVisible();
    expect(docs.getByText("PATH")).toBeVisible();

    const search = within(row("search"));
    expect(search.getByText("https://mcp.example.com/search")).toBeVisible();
    expect(search.getByText("SEARCH_TOKEN")).toBeVisible();

    const repo = within(row("tidebreak"));
    expect(repo.getByText("/Users/alex/src/tidebreak")).toBeVisible();
    expect(
      repo.getByText("https://github.com/brightwave-inc/tidebreak.git"),
    ).toBeVisible();
    expect(repo.getByText("pnpm install")).toBeVisible();
  });

  it("adds only new entries by default and skips conflicts and remaps", async () => {
    const user = userEvent.setup();
    const { applyWorkspaceConfig } = renderSection({
      entries: [
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
          status: "conflict",
          differing_fields: ["args"],
          remap_fields: [],
        },
        {
          section: "mcp_servers",
          key: "search",
          status: "new",
          differing_fields: [],
          remap_fields: [],
        },
      ],
    });
    await importDocument(user);
    await screen.findByLabelText("Import preview");

    // A conflict can be kept or replaced, never added over the record.
    expect(
      within(row("docs")).queryByRole("button", { name: "Add" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(applyWorkspaceConfig).toHaveBeenCalled());
    const actions = applyWorkspaceConfig.mock.calls[0][0].decisions.map(
      (decision) => [decision.key, decision.action],
    );
    expect(actions).toEqual([
      ["https://github.com/brightwave-inc/tidebreak.git", "skip"],
      ["docs", "skip"],
      ["search", "add"],
    ]);
  });

  it("imports a local command server turned off unless you start it", async () => {
    const user = userEvent.setup();
    const { applyWorkspaceConfig } = renderSection({
      entries: [
        {
          section: "mcp_servers",
          key: "docs",
          status: "new",
          differing_fields: [],
          remap_fields: [],
        },
        {
          section: "mcp_servers",
          key: "search",
          status: "new",
          differing_fields: [],
          remap_fields: [],
        },
      ],
    });
    await importDocument(user);
    await screen.findByLabelText("Import preview");

    const start = screen.getByRole("switch", {
      name: "Start docs after import",
    });
    expect(start).not.toBeChecked();
    // A remote server runs nothing on this computer, so it has no switch.
    expect(within(row("search")).queryByRole("switch")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(applyWorkspaceConfig).toHaveBeenCalledTimes(1));
    const [docs, search] = applyWorkspaceConfig.mock.calls[0][0].decisions;
    expect(docs.enabled).toBe(false);
    expect(search.enabled).toBeUndefined();
  });

  it("asks to start a local command server only when you turn it on", async () => {
    const user = userEvent.setup();
    const { applyWorkspaceConfig } = renderSection({
      entries: [
        {
          section: "mcp_servers",
          key: "docs",
          status: "new",
          differing_fields: [],
          remap_fields: [],
        },
      ],
    });
    await importDocument(user);

    await user.click(
      await screen.findByRole("switch", { name: "Start docs after import" }),
    );
    expect(
      screen.getByText(
        "Tidebreak shows you the command and asks before it runs.",
      ),
    ).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(applyWorkspaceConfig).toHaveBeenCalled());
    expect(applyWorkspaceConfig.mock.calls[0][0].decisions[0].enabled).toBe(
      true,
    );
  });
});
