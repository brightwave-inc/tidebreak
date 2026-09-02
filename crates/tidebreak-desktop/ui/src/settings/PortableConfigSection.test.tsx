// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  WorkspaceConfigDocument,
  WorkspaceConfigPreview,
} from "@/api/types";
import { PortableConfigSection } from "./PortableConfigSection";

const document: WorkspaceConfigDocument = {
  tidebreak_config: 1,
  exported_at: "2026-09-02T00:00:00Z",
  sections: {
    code_repositories: [],
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

afterEach(cleanup);

describe("PortableConfigSection", () => {
  it("shows preview statuses before applying", async () => {
    const preview: WorkspaceConfigPreview = {
      entries: [
        {
          section: "mcp_servers",
          key: "docs",
          status: "conflict",
          differing_fields: ["command"],
          remap_fields: ["command"],
        },
      ],
    };
    const previewWorkspaceConfig = vi.fn(async () => preview);
    const applyWorkspaceConfig = vi.fn(async () => ({
      applied: 1,
      skipped: 0,
    }));
    const user = userEvent.setup();
    render(
      <PortableConfigSection
        client={{
          exportWorkspaceConfig: async () => document,
          previewWorkspaceConfig,
          applyWorkspaceConfig,
        }}
      />,
    );

    const file = new File([JSON.stringify(document)], "tidebreak-config.json", {
      type: "application/json",
    });
    await user.upload(
      screen.getByLabelText("Import workspace configuration"),
      file,
    );

    expect(await screen.findByText("Conflicts with an existing record.")).toBeVisible();
    expect(screen.getByText(/Differing: command/)).toBeVisible();
    expect(screen.getByLabelText("Remap command for docs")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Replace" }));
    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(applyWorkspaceConfig).toHaveBeenCalled());
    expect(applyWorkspaceConfig.mock.calls[0][0].decisions[0].action).toBe(
      "replace",
    );
  });
});
