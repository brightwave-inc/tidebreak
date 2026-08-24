import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";
import { useState } from "react";

import type {
  DeliverablePreview,
  DeliverableSummary,
  OutputRevisionInfo,
} from "@/deliverables";
import {
  OutputDetailRoot,
  type OutputDetailApis,
} from "@/outputs/OutputDetailRoot";
import { SourceNavProvider } from "@/panel/SourceNav";

const markdown = `# Q3 renewal plan

The renewal pipeline is healthy, but the next 90 days carry most of the risk.

## Recommended actions

1. Assign an executive sponsor to the five accounts above $250K ARR.
2. Confirm product adoption plans before September 15.
3. Review pricing exceptions with finance each Friday.
`;

const currentPreview: DeliverablePreview = {
  outputId: "renewal-plan",
  filename: "q3-renewal-plan.md",
  mediaType: "text/markdown",
  revisionCount: 3,
  revisionId: "revision-3",
  content: markdown,
  truncated: false,
};

const historicalPreview: DeliverablePreview = {
  ...currentPreview,
  revisionId: "revision-1",
  content: `# Q3 renewal plan\n\nStart with the largest renewals and confirm ownership.`,
};

const revisions: OutputRevisionInfo[] = [
  {
    revisionId: "revision-3",
    ordinal: 3,
    sizeBytes: 8_420,
    createdAt: "2026-08-24T13:40:00.000Z",
    producedBy: "agent",
    isCurrent: true,
    sources: [
      {
        kind: "document",
        citationId: "citation-renewal-table",
        documentId: "document-renewals",
        locator: { kind: "pages", start: 4, end: 6 },
      },
      {
        kind: "web",
        url: "https://www.sec.gov/example",
        label: "Quarterly filing",
        domain: "sec.gov",
      },
    ],
  },
  {
    revisionId: "revision-2",
    ordinal: 2,
    sizeBytes: 7_940,
    createdAt: "2026-08-23T18:15:00.000Z",
    producedBy: "user",
    isCurrent: false,
    sources: [],
  },
  {
    revisionId: "revision-1",
    ordinal: 1,
    sizeBytes: 6_112,
    createdAt: "2026-08-22T15:10:00.000Z",
    producedBy: "agent",
    isCurrent: false,
    sources: [
      {
        kind: "document",
        citationId: "citation-renewal-summary",
        documentId: "document-renewals",
        locator: { kind: "page", page: 2 },
      },
    ],
  },
];

const summary: DeliverableSummary = {
  outputId: currentPreview.outputId,
  filename: currentPreview.filename,
  mediaType: currentPreview.mediaType,
  sizeBytes: 8_420,
  revisionCount: currentPreview.revisionCount,
  updatedAt: "2026-08-24T13:40:00.000Z",
  producingRunId: null,
};

function outputApis(
  overrides: Partial<OutputDetailApis> = {},
): OutputDetailApis {
  return {
    read: async () => currentPreview,
    export: async () => ({
      operationId: "export-renewal-plan",
      outputId: currentPreview.outputId,
      revisionId: currentPreview.revisionId,
      status: "completed",
    }),
    listRevisions: async () => ({
      outputId: currentPreview.outputId,
      revisions,
    }),
    readRevision: async (_chatId, _outputId, revisionId) =>
      revisionId === historicalPreview.revisionId
        ? historicalPreview
        : currentPreview,
    restoreRevision: async () => summary,
    save: async (_chatId, _outputId, _expectedRevisionId, content) => ({
      status: "saved",
      preview: {
        ...currentPreview,
        revisionCount: 4,
        revisionId: "revision-4",
        content,
      },
    }),
    ...overrides,
  };
}

type OutputDetailStoryProps = {
  apis: OutputDetailApis;
};

function OutputDetailStory({ apis }: OutputDetailStoryProps) {
  const [router] = useState(() => {
    const rootRoute = createRootRoute();
    const outputRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: "/c/$chatId",
      component: () => (
        <SourceNavProvider value={{ openCitation: fn(), openDocument: fn() }}>
          <OutputDetailRoot
            chatId="chat-storybook"
            outputId="renewal-plan"
            apis={apis}
          />
        </SourceNavProvider>
      ),
    });
    return createRouter({
      routeTree: rootRoute.addChildren([outputRoute]),
      history: createMemoryHistory({
        initialEntries: ["/c/chat-storybook"],
      }),
    });
  });

  return (
    <div className="h-screen min-h-0 bg-page-background text-foreground">
      <RouterProvider router={router as never} />
    </div>
  );
}

const meta = {
  title: "Outputs/Detail",
  component: OutputDetailStory,
  parameters: { layout: "fullscreen" },
  args: { apis: outputApis() },
  render: (args) => <OutputDetailStory key={String(args.apis)} {...args} />,
} satisfies Meta<typeof OutputDetailStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const CurrentRevision: Story = {};

export const Loading: Story = {
  args: {
    apis: outputApis({
      read: async () => new Promise<DeliverablePreview>(() => undefined),
      listRevisions: async () => new Promise(() => undefined),
    }),
  },
};

export const Failure: Story = {
  args: {
    apis: outputApis({
      read: async () => {
        throw new Error("The output revision is unavailable.");
      },
    }),
  },
};

export const VersionHistoryOpen: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Version history" }),
    );
    await expect(await canvas.findByText("Current version")).toBeVisible();
  },
};

export const HistoricalRevision: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Version history" }),
    );
    await userEvent.click(await canvas.findByRole("button", { name: /v1/i }));
    await expect(await canvas.findByText(/Viewing v1/)).toBeVisible();
  },
};

export const EditMode: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByRole("button", { name: "Edit" }));
    await expect(
      await canvas.findByRole("textbox", { name: /Edit q3-renewal-plan/ }),
    ).toBeVisible();
  },
};

export const EditConflict: Story = {
  args: {
    apis: outputApis({
      save: async () => ({
        status: "conflict",
        currentRevisionId: "revision-4",
      }),
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByRole("button", { name: "Edit" }));
    const editor = await canvas.findByRole("textbox", {
      name: /Edit q3-renewal-plan/,
    });
    await userEvent.type(editor, "\nOwner: Customer success");
    await userEvent.click(canvas.getByRole("button", { name: "Save" }));
    await expect(await canvas.findByRole("alert")).toBeVisible();
  },
};

export const Compact: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};
