import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor, within } from "storybook/test";

import type { DeliverablesCatalog, DeliverableSummary } from "@/deliverables";
import { OutputsView, type OutputsApis } from "@/outputs/OutputsView";

const BASE_TIME = Date.parse("2026-08-21T15:30:00.000Z");

const outputKinds = [
  ["Board update.md", "text/markdown", 18_420],
  ["Pipeline health.csv", "text/csv", 82_114],
  ["Revenue mix.chart.json", "application/vnd.tidebreak.chart+json", 4_812],
  ["Account segments.json", "application/json", 36_720],
  ["Launch checklist.txt", "text/plain", 6_904],
  ["Pricing analysis.html", "text/html", 24_612],
] as const;

const denseOutputs: DeliverableSummary[] = Array.from(
  { length: 24 },
  (_, index) => {
    const [filename, mediaType, sizeBytes] =
      outputKinds[index % outputKinds.length]!;
    const copy = Math.floor(index / outputKinds.length);
    return {
      outputId: `output-${index + 1}`,
      filename: copy === 0 ? filename : filename.replace(".", ` ${copy + 1}.`),
      mediaType,
      sizeBytes: sizeBytes + index * 977,
      revisionCount: index % 5 === 0 ? 4 : index % 3 === 0 ? 2 : 1,
      updatedAt: new Date(BASE_TIME - index * 37 * 60_000).toISOString(),
      producingRunId: index % 4 === 0 ? `run-${index + 1}` : null,
    };
  },
);

function catalogApis(catalog: DeliverablesCatalog): OutputsApis {
  return {
    list: async () => catalog,
    export: async (_chatId, outputId) => ({
      operationId: `export-${outputId}`,
      outputId,
      revisionId: `revision-${outputId}`,
      status: "completed",
    }),
    delete: async (_chatId, outputId) =>
      catalog.deliverables.find((output) => output.outputId === outputId)!,
    restore: async (_chatId, outputId) =>
      catalog.deliverables.find((output) => output.outputId === outputId)!,
  };
}

const loadingApis: OutputsApis = {
  list: async () => new Promise<DeliverablesCatalog>(() => undefined),
  export: async () => new Promise(() => undefined),
  delete: async () => new Promise(() => undefined),
  restore: async () => new Promise(() => undefined),
};

const failureApis: OutputsApis = {
  ...catalogApis({ deliverables: [], truncated: false }),
  list: async () => {
    throw new Error("The output catalog did not respond.");
  },
};

const meta = {
  title: "Outputs/Catalog",
  component: OutputsView,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => (
      <div className="flex h-screen min-h-0 bg-page-background text-foreground">
        <Story />
      </div>
    ),
  ],
  args: {
    chatId: "chat-storybook",
    onOpen: fn(),
    apis: catalogApis({ deliverables: denseOutputs, truncated: false }),
  },
  render: (args) => <OutputsView key={String(args.apis)} {...args} />,
} satisfies Meta<typeof OutputsView>;

export default meta;
type Story = StoryObj<typeof meta>;

export const DenseCatalog: Story = {
  play: async ({ canvasElement }) => {
    const grid = canvasElement.querySelector(".ag-root-wrapper");
    if (!(grid instanceof HTMLElement)) {
      throw new Error("The outputs grid did not mount.");
    }
    await waitFor(() => {
      expect(grid.getBoundingClientRect().height).toBeGreaterThan(300);
    });
    await expect(
      within(canvasElement).getByRole("button", {
        name: "Open Board update.md",
      }),
    ).toBeVisible();
  },
};

export const Loading: Story = {
  args: { apis: loadingApis },
};

export const Empty: Story = {
  args: {
    apis: catalogApis({ deliverables: [], truncated: false }),
  },
};

export const Failure: Story = {
  args: { apis: failureApis },
};

export const TruncatedCatalog: Story = {
  args: {
    apis: catalogApis({ deliverables: denseOutputs, truncated: true }),
  },
};

export const NoMatchingResults: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const search = await canvas.findByPlaceholderText("Search outputs…");
    await userEvent.type(search, "quarterly board deck");
    await expect(
      canvas.getByText("No outputs match your search."),
    ).toBeVisible();
  },
};

export const Compact: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};
