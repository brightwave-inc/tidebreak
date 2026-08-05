// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PluginCatalog } from "@/api";
import { PluginDetailView } from "./PluginDetailView";
import type { PluginsApis } from "./pluginsApis";
import { PluginsView } from "./PluginsView";
import { usePluginCatalog } from "./usePluginCatalog";

vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

const CATALOG: PluginCatalog = {
  plugins: [
    {
      name: "documents",
      display_name: "Documents",
      description: "Write and revise Word documents.",
      category: "documents",
      capabilities: ["write-files", "host-install"],
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
};

function catalogWith(
  plugins: Partial<Record<string, boolean>>,
): PluginCatalog {
  return {
    ...CATALOG,
    plugins: CATALOG.plugins.map((plugin) => ({
      ...plugin,
      enabled: plugins[plugin.name] ?? plugin.enabled,
    })),
  };
}

/** Drives the real catalog hook so a toggle exercises the whole round trip. */
function ListHarness({ apis }: { apis: PluginsApis }) {
  const state = usePluginCatalog(apis);
  return <PluginsView state={state} onOpen={() => {}} />;
}

function DetailHarness({ apis }: { apis: PluginsApis }) {
  const state = usePluginCatalog(apis);
  return (
    <PluginDetailView pluginId="documents" state={state} onBack={() => {}} />
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Plugins library", () => {
  it("gates a disabled plugin's member switches without clearing their own flags", async () => {
    const apis: PluginsApis = {
      list: vi.fn().mockResolvedValue(CATALOG),
      setEnabled: vi.fn(),
    };
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
    const apis: PluginsApis = {
      list: vi.fn().mockResolvedValue(CATALOG),
      setEnabled: vi.fn().mockResolvedValue(catalogWith({ documents: true })),
    };
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
    const apis: PluginsApis = {
      list: vi.fn().mockResolvedValue(CATALOG),
      setEnabled: vi.fn().mockResolvedValue(reconciled),
    };
    render(<ListHarness apis={apis} />);

    const bundle = await screen.findByRole("switch", { name: "Enable Documents" });
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
    const apis: PluginsApis = {
      list: vi.fn().mockResolvedValue(CATALOG),
      setEnabled: vi.fn().mockRejectedValue(new Error("offline")),
    };
    render(<ListHarness apis={apis} />);

    const skill = await screen.findByRole("switch", { name: "Enable my-notes" });
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

  it("explains an installation with nothing in it", async () => {
    const apis: PluginsApis = {
      list: vi.fn().mockResolvedValue({ plugins: [], skills: [] }),
      setEnabled: vi.fn(),
    };
    render(<ListHarness apis={apis} />);

    expect(await screen.findByText("No plugins installed")).toBeInTheDocument();
    // The user-skills section is absent rather than empty when there are none.
    expect(screen.queryByLabelText("Your skills")).not.toBeInTheDocument();
  });
});
