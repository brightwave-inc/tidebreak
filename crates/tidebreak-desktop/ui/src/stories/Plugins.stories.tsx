import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import type { PluginCatalog, PluginSkillInfo } from "@/api";
import { PluginDetailView } from "@/plugins/PluginDetailView";
import { PluginsView } from "@/plugins/PluginsView";
import { SkillDialog } from "@/plugins/SkillDialog";
import type { PluginCatalogState } from "@/plugins/usePluginCatalog";

const documentSkills: PluginSkillInfo[] = [
  {
    name: "documents",
    description: "Create, edit, and review Word documents.",
    origin: "builtin",
    enabled: true,
  },
  {
    name: "pdf",
    description: "Read, create, render, and inspect PDF files.",
    origin: "builtin",
    enabled: true,
  },
  {
    name: "presentations",
    description: "Build and revise presentation decks.",
    origin: "builtin",
    enabled: false,
  },
];

const catalog: PluginCatalog = {
  plugins: [
    {
      name: "document-work",
      display_name: "Document work",
      description: "Create, inspect, and revise office documents and PDFs.",
      category: "documents",
      origin: "builtin",
      capabilities: ["write-files"],
      compatibility: { status: "compatible", issues: [] },
      enabled: true,
      skills: documentSkills,
    },
    {
      name: "data-workbench",
      display_name: "Data workbench",
      description: "Analyze spreadsheets and produce verified data outputs.",
      category: "data",
      origin: "builtin",
      capabilities: ["write-files", "host-install"],
      compatibility: {
        status: "limited",
        issues: [
          {
            kind: "missing_sandbox_dependency",
            skill: "spreadsheets",
            dependency: "libreoffice",
          },
        ],
      },
      enabled: false,
      skills: [
        {
          name: "spreadsheets",
          description: "Create, edit, analyze, and verify spreadsheet files.",
          origin: "builtin",
          enabled: true,
        },
      ],
    },
    {
      name: "browser-research",
      display_name: "Browser research",
      description: "Inspect websites and collect evidence from live sources.",
      category: "other",
      origin: "user",
      capabilities: ["network", "live-control", "mcp"],
      compatibility: { status: "unchecked", issues: [] },
      enabled: true,
      skills: [
        {
          name: "browser-audit",
          description: "Review a web flow with screenshots and notes.",
          origin: "user",
          enabled: true,
        },
      ],
    },
    {
      name: "visual-studio",
      display_name: "Visual studio",
      description: "Generate visual references and interactive diagrams.",
      category: "visualization",
      origin: "builtin",
      capabilities: [],
      compatibility: { status: "compatible", issues: [] },
      enabled: true,
      skills: [],
    },
  ],
  skills: [
    {
      name: "release-notes",
      description: "Turn a set of merged changes into release notes.",
      origin: "user",
      enabled: true,
    },
    {
      name: "accessibility-review",
      description: "Check interaction states and keyboard access.",
      origin: "builtin",
      enabled: false,
    },
  ],
  prompts: [],
};

function state(values: Partial<PluginCatalogState> = {}): PluginCatalogState {
  return {
    catalog,
    loading: false,
    error: null,
    reload: fn(),
    setEnabled: fn(),
    ...values,
  };
}

const loadInstructions = async (name: string) => ({
  name,
  instructions: `# Review a document

Read the source once before you edit it.

## Checklist

- Keep the document's voice.
- Preserve facts and links.
- Render the final document and inspect every page.
`,
});

const meta = {
  title: "Plugins/Library",
  component: PluginsView,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => (
      <div className="flex h-screen min-h-0 bg-page-background">
        <Story />
      </div>
    ),
  ],
  args: {
    state: state(),
    loadInstructions,
    onOpen: fn(),
  },
} satisfies Meta<typeof PluginsView>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  args: { state: state({ catalog: null, loading: true }) },
};

export const Empty: Story = {
  args: {
    state: state({ catalog: { plugins: [], skills: [], prompts: [] } }),
  },
};

export const DenseCatalog: Story = {};

const importedSkillNames = [
  "incident-review",
  "customer-support-incident-review-with-a-long-unbroken-skill-name",
];

const importReport = {
  imported: importedSkillNames,
  skipped: [
    {
      name: "draft-helper",
      reason: "No regular SKILL.md was found",
    },
  ],
  conflicts: [
    {
      name: "documents",
      reason: "A skill included with Tidebreak already uses this name",
    },
  ],
};

const catalogAfterImport: PluginCatalog = {
  ...catalog,
  skills: [
    ...catalog.skills,
    ...importedSkillNames.map((name) => ({
      name,
      description: "Imported from a local skill folder.",
      origin: "user" as const,
      enabled: true,
    })),
  ],
};

function ImportResultsStory() {
  const [storyCatalog, setStoryCatalog] = useState(catalog);
  return (
    <PluginsView
      state={state({
        catalog: storyCatalog,
        reload: () => setStoryCatalog(catalogAfterImport),
      })}
      loadInstructions={loadInstructions}
      onOpen={fn()}
      importSkills={async () => importReport}
    />
  );
}

const importResultsPlay = async (canvasElement: HTMLElement) => {
  const canvas = within(canvasElement);
  await userEvent.click(
    await canvas.findByRole("button", { name: "Import skills" }),
  );
  const summary = await canvas.findByText("Skill import complete");
  await expect(summary).toBeInTheDocument();
  await expect(await canvas.findAllByText("incident-review")).toHaveLength(2);
  summary.scrollIntoView({ block: "center" });
};

export const ImportResults: Story = {
  render: () => <ImportResultsStory />,
  play: ({ canvasElement }) => importResultsPlay(canvasElement),
};

export const ImportResultsCompact: Story = {
  render: () => <ImportResultsStory />,
  parameters: { viewport: { defaultViewport: "compact" } },
  play: ({ canvasElement }) => importResultsPlay(canvasElement),
};

export const LoadFailure: Story = {
  args: {
    state: state({
      catalog: null,
      error: "The plugin catalog did not answer.",
    }),
  },
};

export const EnabledPlugin: Story = {
  render: () => (
    <PluginDetailView
      pluginId="document-work"
      state={state()}
      loadInstructions={loadInstructions}
      onBack={fn()}
    />
  ),
};

export const DisabledPlugin: Story = {
  render: () => (
    <PluginDetailView
      pluginId="data-workbench"
      state={state()}
      loadInstructions={loadInstructions}
      onBack={fn()}
    />
  ),
};

export const MissingPlugin: Story = {
  render: () => (
    <PluginDetailView
      pluginId="not-installed"
      state={state()}
      loadInstructions={loadInstructions}
      onBack={fn()}
    />
  ),
};

export const SkillInstructions: Story = {
  render: () => (
    <SkillDialog
      skill={documentSkills[0]}
      gated={false}
      onOpenChange={fn()}
      onToggle={fn()}
      loadInstructions={loadInstructions}
    />
  ),
};

export const SkillInstructionsLoading: Story = {
  render: () => (
    <SkillDialog
      skill={documentSkills[0]}
      gated={false}
      onOpenChange={fn()}
      onToggle={fn()}
      loadInstructions={() => new Promise(() => undefined)}
    />
  ),
};

export const SkillInstructionsFailure: Story = {
  render: () => (
    <SkillDialog
      skill={documentSkills[0]}
      gated
      onOpenChange={fn()}
      onToggle={fn()}
      loadInstructions={async () => {
        throw new Error("instructions unavailable");
      }}
    />
  ),
};
