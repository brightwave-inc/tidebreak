import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import type { OutputRevisionSource } from "@/deliverables";
import { OutputRevisionSources } from "@/outputs/OutputRevisionSources";

function RevisionPanel({ sources }: { sources: OutputRevisionSource[] }) {
  return (
    <div className="flex h-80 flex-col overflow-hidden rounded-lg border bg-page-background">
      <div className="min-h-0 flex-1 overflow-auto p-6">
        <article className="mx-auto max-w-4xl text-sm">
          <h1 className="mb-3 text-xl font-semibold">
            Quarterly revenue brief
          </h1>
          <p>Revenue increased, led by enterprise expansion.</p>
        </article>
      </div>
      <OutputRevisionSources
        sources={sources}
        onOpenDocument={fn()}
        onOpenWeb={fn()}
      />
    </div>
  );
}

const sourcedRevision: OutputRevisionSource[] = [
  {
    kind: "document",
    citationId: "46abf484-8368-4c2d-b2ec-8b9ed77e202f",
    documentId: "4571ebc0-69a7-4f8a-a9c7-936c50f0f022",
    locator: { kind: "pages", start: 4, end: 6 },
  },
  {
    kind: "web",
    url: "https://www.sec.gov/example",
    label: "Quarterly filing",
    domain: "sec.gov",
  },
];

const meta = {
  title: "Outputs/Revision sources",
  component: RevisionPanel,
  args: { sources: sourcedRevision },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-4xl p-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof RevisionPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A foreground revision shows only the evidence its producing turn retrieved. */
export const WithDocumentAndWebEvidence: Story = {};

/** User edits and background-agent revisions do not borrow another turn's sources. */
export const WithoutTurnEvidence: Story = {
  args: { sources: [] },
};
