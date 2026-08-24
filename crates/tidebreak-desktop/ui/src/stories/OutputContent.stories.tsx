import type { Meta, StoryObj } from "@storybook/react-vite";

import type { DeliverablePreview } from "@/deliverables";
import { OutputContent } from "@/outputs/OutputContent";

const markdown = `# Renewal analysis

Enterprise renewals remain the main driver of expansion revenue.

## Recommendation

Prioritize the 18 accounts with adoption above 70% and renewal dates inside 90 days.

| Segment | Accounts | Expansion potential |
| --- | ---: | ---: |
| Enterprise | 18 | $1.42M |
| Growth | 31 | $684K |
| Emerging | 47 | $212K |
`;

const code = `{
  "generated_at": "2026-08-21T15:30:00Z",
  "segments": [
    { "name": "Enterprise", "accounts": 18, "pipeline": 1420000 },
    { "name": "Growth", "accounts": 31, "pipeline": 684000 },
    { "name": "Emerging", "accounts": 47, "pipeline": 212000 }
  ]
}`;

const chart = JSON.stringify({
  data: [
    {
      type: "bar",
      x: ["Enterprise", "Growth", "Emerging"],
      y: [1.42, 0.684, 0.212],
      marker: { color: ["#6f9277", "#96aa9a", "#c4cec5"] },
      hovertemplate: "%{x}<br>$%{y:.3f}M<extra></extra>",
    },
  ],
  layout: {
    title: { text: "Expansion pipeline by segment", x: 0 },
    yaxis: { title: "Pipeline ($M)", rangemode: "tozero" },
    margin: { t: 64, r: 24, b: 56, l: 64 },
  },
});

function preview(
  mediaType: string,
  content: string,
  options: Partial<DeliverablePreview> = {},
): DeliverablePreview {
  return {
    outputId: "output-storybook",
    filename: "renewal-analysis.md",
    mediaType,
    revisionCount: 3,
    revisionId: "revision-3",
    content,
    truncated: false,
    ...options,
  };
}

function ContentStory({ preview }: { preview: DeliverablePreview }) {
  return (
    <div className="flex h-[680px] min-h-0 overflow-hidden rounded-lg border bg-page-background">
      <OutputContent chatId="chat-storybook" preview={preview} />
    </div>
  );
}

const meta = {
  title: "Outputs/Content",
  component: ContentStory,
  args: { preview: preview("text/markdown", markdown) },
} satisfies Meta<typeof ContentStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const MarkdownReport: Story = {};

export const CodeAndData: Story = {
  args: {
    preview: preview("application/json", code, {
      filename: "account-segments.json",
    }),
  },
};

export const Chart: Story = {
  args: {
    preview: preview("application/vnd.tidebreak.chart+json", chart, {
      filename: "expansion-pipeline.chart.json",
    }),
  },
};

export const InvalidChartSource: Story = {
  args: {
    preview: preview(
      "application/vnd.tidebreak.chart+json",
      '{"data":[],"layout":{"title":"Missing traces"}}',
      { filename: "invalid.chart.json" },
    ),
  },
};

export const TruncatedChartSource: Story = {
  args: {
    preview: preview(
      "application/vnd.tidebreak.chart+json",
      chart.slice(0, 180),
      {
        filename: "large-chart.chart.json",
        truncated: true,
      },
    ),
  },
};

export const UnsupportedFormat: Story = {
  args: {
    preview: preview("application/zip", "", {
      filename: "research-archive.zip",
    }),
  },
};

export const TruncatedText: Story = {
  args: {
    preview: preview("text/markdown", markdown, { truncated: true }),
  },
};

export const CompactCode: Story = {
  args: {
    preview: preview("application/json", code, {
      filename: "account-segments.json",
    }),
  },
  globals: { viewport: { value: "compact", isRotated: false } },
};
