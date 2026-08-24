import type { Meta, StoryObj } from "@storybook/react-vite";

import { ImageViewer } from "@/components/document/image-viewer";
import { JsonViewer } from "@/components/document/json-viewer";
import { MarkdownViewer } from "@/components/document/markdown-viewer";
import { XmlViewer } from "@/components/document/xml-viewer";
import type { FileBytesSource } from "@/document/useFileDownload";

type ViewerVariant =
  | "markdown"
  | "plain-citation"
  | "json"
  | "invalid-json"
  | "xml"
  | "unsupported"
  | "download-progress"
  | "download-failure"
  | "image";

const markdown = `# Customer evidence review

This source combines interview notes, usage data, and renewal context.

## What changed

Enterprise teams adopted shared workspaces faster after the permissions update.

## Evidence

1. Weekly active collaborators increased from 4.2 to 7.8.
2. Review turnaround fell from 31 hours to 18 hours.
3. Renewal risk moved from high to medium for three accounts.

## Next questions

- Which teams still depend on exported files?
- Where does review ownership remain unclear?
`;

const plainText = `Interview notes
Participant: Operations lead
Date: August 19, 2026

The permissions update reduced the number of manual exports.
Reviewers now stay inside the workspace for most approval cycles.
Two teams still export spreadsheets because their finance partners do not have access.

Follow-up
Confirm whether guest access covers the finance review workflow.`;

const json = JSON.stringify(
  {
    account: {
      name: "Northstar Health",
      renewal: {
        date: "2026-10-15",
        annualRecurringRevenue: 428_000,
        risk: "medium",
      },
      adoption: {
        activeUsers: 184,
        weeklyCollaborators: 7.8,
        workflows: ["Research", "Review", "Executive briefing"],
      },
      owners: [
        { function: "Customer success", name: "Mara Chen" },
        { function: "Executive sponsor", name: "Devon Price" },
      ],
    },
  },
  null,
  2,
);

const xml = `<?xml version="1.0" encoding="UTF-8"?>
<renewal-plan account="Northstar Health">
  <summary risk="medium" annual-recurring-revenue="428000" />
  <owners>
    <owner function="customer-success">Mara Chen</owner>
    <owner function="executive-sponsor">Devon Price</owner>
  </owners>
  <actions>
    <action due="2026-09-05">Confirm legal review owner</action>
    <action due="2026-09-12">Approve pricing exception</action>
  </actions>
</renewal-plan>`;

const imageSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="960" height="540" viewBox="0 0 960 540">
  <rect width="960" height="540" fill="#e7ebe5"/>
  <rect x="56" y="52" width="848" height="436" rx="18" fill="#fbfcfa" stroke="#c9d1c8"/>
  <text x="96" y="116" font-family="system-ui, sans-serif" font-size="22" font-weight="600" fill="#243027">Renewal health by account</text>
  <text x="96" y="148" font-family="system-ui, sans-serif" font-size="13" fill="#718076">90-day operating review</text>
  <line x1="96" y1="424" x2="848" y2="424" stroke="#c9d1c8"/>
  <rect x="132" y="226" width="88" height="198" rx="6" fill="#688b70"/>
  <rect x="278" y="276" width="88" height="148" rx="6" fill="#86a08b"/>
  <rect x="424" y="310" width="88" height="114" rx="6" fill="#9fb3a2"/>
  <rect x="570" y="338" width="88" height="86" rx="6" fill="#b8c5b9"/>
  <rect x="716" y="366" width="88" height="58" rx="6" fill="#cad3cb"/>
  <text x="138" y="454" font-family="system-ui, sans-serif" font-size="12" fill="#526157">Northstar</text>
  <text x="293" y="454" font-family="system-ui, sans-serif" font-size="12" fill="#526157">Fieldline</text>
  <text x="439" y="454" font-family="system-ui, sans-serif" font-size="12" fill="#526157">Juniper</text>
  <text x="584" y="454" font-family="system-ui, sans-serif" font-size="12" fill="#526157">Harbor</text>
  <text x="732" y="454" font-family="system-ui, sans-serif" font-size="12" fill="#526157">Canopy</text>
</svg>`;

function source(
  id: string,
  content: string,
  contentType: string,
): FileBytesSource {
  return {
    id,
    cacheKey: `storybook/${id}`,
    fetch: async () => ({
      bytes: new TextEncoder().encode(content),
      contentType,
    }),
  };
}

const progressSource: FileBytesSource = {
  id: "progress",
  cacheKey: "storybook/progress",
  fetch: async (_signal, onProgress) => {
    onProgress?.({
      loaded: 12.6 * 1024 * 1024,
      total: 28.4 * 1024 * 1024,
      percentage: 44.4,
    });
    return new Promise(() => undefined);
  },
};

const failureSource: FileBytesSource = {
  id: "failure",
  cacheKey: "storybook/failure",
  fetch: async () => {
    throw new Error("The source download was interrupted.");
  },
};

function DocumentViewerStory({ variant }: { variant: ViewerVariant }) {
  const className = "h-[680px] min-h-0 rounded-lg border bg-page-background";

  switch (variant) {
    case "markdown":
      return (
        <MarkdownViewer
          source={source("markdown", markdown, "text/markdown")}
          markdown
          className={className}
        />
      );
    case "plain-citation":
      return (
        <MarkdownViewer
          source={source("plain", plainText, "text/plain")}
          targetLines={{ start: 5, end: 7 }}
          className={className}
        />
      );
    case "json":
      return (
        <JsonViewer
          source={source("json", json, "application/json")}
          highlightPath="account.renewal.risk"
          className={className}
        />
      );
    case "invalid-json":
      return (
        <JsonViewer
          source={source(
            "invalid-json",
            "{ not valid json",
            "application/json",
          )}
          className={className}
        />
      );
    case "xml":
      return (
        <XmlViewer
          source={source("xml", xml, "application/xml")}
          highlightPath="/renewal-plan/actions/action[2]"
          className={className}
        />
      );
    case "unsupported":
      return (
        <MarkdownViewer
          source={source(
            "unsupported",
            "Binary bytes are not rendered as text.",
            "application/octet-stream",
          )}
          className={className}
        />
      );
    case "download-progress":
      return <MarkdownViewer source={progressSource} className={className} />;
    case "download-failure":
      return <MarkdownViewer source={failureSource} className={className} />;
    case "image":
      return (
        <ImageViewer
          source={source("image", imageSvg, "image/svg+xml")}
          className={className}
        />
      );
  }
}

const meta = {
  title: "Documents/Viewer content",
  component: DocumentViewerStory,
  args: { variant: "markdown" },
  argTypes: {
    variant: {
      control: "select",
      options: [
        "markdown",
        "plain-citation",
        "json",
        "invalid-json",
        "xml",
        "unsupported",
        "download-progress",
        "download-failure",
        "image",
      ],
    },
  },
} satisfies Meta<typeof DocumentViewerStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const MarkdownWithOutline: Story = {};

export const PlainTextCitation: Story = {
  args: { variant: "plain-citation" },
};

export const JsonTree: Story = {
  args: { variant: "json" },
};

export const InvalidJson: Story = {
  args: { variant: "invalid-json" },
};

export const XmlTree: Story = {
  args: { variant: "xml" },
};

export const UnsupportedFormat: Story = {
  args: { variant: "unsupported" },
};

export const DownloadProgress: Story = {
  args: { variant: "download-progress" },
};

export const DownloadFailure: Story = {
  args: { variant: "download-failure" },
};

export const Image: Story = {
  args: { variant: "image" },
};

export const CompactJson: Story = {
  args: { variant: "json" },
  globals: { viewport: { value: "compact", isRotated: false } },
};
