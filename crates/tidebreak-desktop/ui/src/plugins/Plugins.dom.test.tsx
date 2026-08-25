// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PluginCatalog } from "@/api";
import { PluginDetailView } from "./PluginDetailView";
import type { PluginsApis } from "./pluginsApis";
import { PluginsView } from "./PluginsView";
import type { SkillImportReport } from "./skillImport";
import { usePluginCatalog } from "./usePluginCatalog";

vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

const CATALOG: PluginCatalog = {
  plugins: [
    {
      name: "documents",
      display_name: "Documents",
      description: "Write and revise Word documents.",
      category: "documents",
      origin: "builtin",
      capabilities: ["write-files", "host-install"],
      compatibility: { status: "unchecked", issues: [] },
      enabled: false,
      skills: [
        {
          name: "docx",
          description: "Author a Word document.",
          origin: "builtin",
          enabled: true,
        },
        {
          name: "pdf",
          description: "Author a PDF.",
          origin: "builtin",
          enabled: false,
        },
      ],
    },
  ],
  skills: [
    {
      name: "my-notes",
      description: "How I like meeting notes written.",
      origin: "user",
      enabled: true,
    },
  ],
  prompts: [],
};

function catalogWith(plugins: Partial<Record<string, boolean>>): PluginCatalog {
  return {
    ...CATALOG,
    plugins: CATALOG.plugins.map((plugin) => ({
      ...plugin,
      enabled: plugins[plugin.name] ?? plugin.enabled,
    })),
  };
}

const SKILL_BODY = "Use this skill when authoring Word documents.";

function apisWith(overrides: Partial<PluginsApis> = {}): PluginsApis {
  return {
    list: vi.fn().mockResolvedValue(CATALOG),
    setEnabled: vi.fn(),
    instructions: vi.fn().mockImplementation(async (name: string) => ({
      name,
      instructions: SKILL_BODY,
    })),
    promptBody: vi.fn(),
    ...overrides,
  };
}

/** Drives the real catalog hook so a toggle exercises the whole round trip. */
function ListHarness({
  apis,
  importSkills,
}: {
  apis: PluginsApis;
  importSkills?: () => Promise<SkillImportReport | null>;
}) {
  const state = usePluginCatalog(apis);
  return (
    <PluginsView
      state={state}
      loadInstructions={apis.instructions}
      onOpen={() => {}}
      importSkills={importSkills}
    />
  );
}

function DetailHarness({ apis }: { apis: PluginsApis }) {
  const state = usePluginCatalog(apis);
  return (
    <PluginDetailView
      pluginId="documents"
      state={state}
      loadInstructions={apis.instructions}
      onBack={() => {}}
    />
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Plugins library", () => {
  it("gates a disabled plugin's member switches without clearing their own flags", async () => {
    const apis = apisWith();
    render(<DetailHarness apis={apis} />);

    // The bundle is off, so no member can run whatever its own flag says —
    // but the flags are still what the server holds, so the switches show the
    // choices that come back when the bundle does.
    const docx = await screen.findByRole("switch", { name: "Enable docx" });
    const pdf = screen.getByRole("switch", { name: "Enable pdf" });
    expect(docx).toBeDisabled();
    expect(pdf).toBeDisabled();
    expect(docx).toBeChecked();
    expect(pdf).not.toBeChecked();

    // Capability badges read as sentences on the detail view.
    expect(screen.getByText("Writes files")).toBeInTheDocument();
    expect(screen.getByText("Installs host software")).toBeInTheDocument();
  });

  it("ungates the members once the plugin's own toggle round trip lands", async () => {
    const apis = apisWith({
      setEnabled: vi.fn().mockResolvedValue(catalogWith({ documents: true })),
    });
    render(<DetailHarness apis={apis} />);

    fireEvent.click(
      await screen.findByRole("switch", { name: "Enable Documents" }),
    );
    expect(apis.setEnabled).toHaveBeenCalledWith({
      plugins: { documents: true },
      skills: {},
    });
    await waitFor(() =>
      expect(screen.getByRole("switch", { name: "Enable docx" })).toBeEnabled(),
    );
  });

  it("reconciles a list toggle from the catalog the server returns", async () => {
    // The server answers with more than the toggle asked for — the user skill
    // comes back off too — and the surface takes that as the truth rather than
    // keeping its optimistic guess.
    const reconciled: PluginCatalog = {
      ...catalogWith({ documents: true }),
      skills: [{ ...CATALOG.skills[0]!, enabled: false }],
    };
    const apis = apisWith({
      setEnabled: vi.fn().mockResolvedValue(reconciled),
    });
    render(<ListHarness apis={apis} />);

    const bundle = await screen.findByRole("switch", {
      name: "Enable Documents",
    });
    fireEvent.click(bundle);
    // Optimistic: the switch is on before the request resolves.
    expect(bundle).toBeChecked();
    await waitFor(() =>
      expect(
        screen.getByRole("switch", { name: "Enable my-notes" }),
      ).not.toBeChecked(),
    );
    expect(bundle).toBeChecked();
  });

  it("puts a failed toggle back where it was", async () => {
    const apis = apisWith({
      setEnabled: vi.fn().mockRejectedValue(new Error("offline")),
    });
    render(<ListHarness apis={apis} />);

    const skill = await screen.findByRole("switch", {
      name: "Enable my-notes",
    });
    expect(skill).toBeChecked();
    fireEvent.click(skill);
    // Optimistic first, then back where it was once the write fails: the
    // surface never keeps claiming a state the server did not record.
    expect(skill).not.toBeChecked();
    await waitFor(() => expect(skill).toBeChecked());
    expect(apis.setEnabled).toHaveBeenCalledWith({
      plugins: {},
      skills: { "my-notes": false },
    });
  });

  it("opens a skill's own instructions from its row", async () => {
    const apis = apisWith();
    render(<DetailHarness apis={apis} />);

    // The row body — not the switch beside it — opens the skill.
    fireEvent.click(await screen.findByRole("button", { name: /docx/ }));

    // The dialog shows the staged instruction body, fetched on open.
    expect(await screen.findByText(SKILL_BODY)).toBeInTheDocument();
    expect(apis.instructions).toHaveBeenCalledWith("docx");
    // Its switch is the same gated control the row has: the bundle is off.
    // (The modal hides the page behind it, so this is the dialog's own.)
    expect(screen.getByRole("switch", { name: "Enable docx" })).toBeDisabled();
  });

  it("explains an installation with nothing in it", async () => {
    const apis = apisWith({
      list: vi.fn().mockResolvedValue({ plugins: [], skills: [] }),
    });
    render(<ListHarness apis={apis} />);

    expect(await screen.findByText("No plugins installed")).toBeInTheDocument();
    // The user-skills section is absent rather than empty when there are none.
    expect(screen.queryByLabelText("Your skills")).not.toBeInTheDocument();
  });

  it("reports every import outcome and reloads the catalog", async () => {
    const apis = apisWith();
    const importedName =
      "customer-support-incident-review-with-a-long-unbroken-skill-name";
    const importSkills = vi.fn().mockResolvedValue({
      imported: [importedName],
      skipped: [
        { name: "draft-helper", reason: "No regular SKILL.md was found" },
      ],
      conflicts: [
        {
          name: "documents",
          reason: "A skill included with Tidebreak already uses this name",
        },
      ],
    } satisfies SkillImportReport);
    render(<ListHarness apis={apis} importSkills={importSkills} />);

    fireEvent.click(
      await screen.findByRole("button", { name: "Import skills" }),
    );

    expect(
      await screen.findByText("Skill import complete"),
    ).toBeInTheDocument();
    const status = screen.getByRole("status");
    expect(status).toHaveTextContent(
      "1 skill imported, 1 skipped, and 1 conflict",
    );
    expect(status).not.toHaveTextContent(importedName);
    expect(screen.getByText(importedName)).toHaveClass("break-all");
    expect(screen.getByText("draft-helper")).toBeInTheDocument();
    expect(screen.getAllByText("documents").length).toBeGreaterThan(0);
    expect(
      screen.getByText("No regular SKILL.md was found"),
    ).toBeInTheDocument();
    await waitFor(() => expect(apis.list).toHaveBeenCalledTimes(2));
  });

  it("leaves the catalog alone when the folder picker is cancelled", async () => {
    const apis = apisWith();
    const importSkills = vi.fn().mockResolvedValue(null);
    render(<ListHarness apis={apis} importSkills={importSkills} />);

    fireEvent.click(
      await screen.findByRole("button", { name: "Import skills" }),
    );

    await waitFor(() => expect(importSkills).toHaveBeenCalledOnce());
    expect(apis.list).toHaveBeenCalledOnce();
    expect(screen.queryByText("Skill import complete")).not.toBeInTheDocument();
    expect(screen.queryByText("No skills imported")).not.toBeInTheDocument();
  });
});
