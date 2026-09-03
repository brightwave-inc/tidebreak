import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import {
  TranscriptFileAttachments,
  type TranscriptFileAttachment,
} from "@/TranscriptFileAttachments";
import { SourceNavProvider } from "@/panel/SourceNav";

const files: TranscriptFileAttachment[] = [
  {
    documentId: "document-board-packet",
    name: "Q3 board packet.pdf",
    mediaType: "application/pdf",
  },
  {
    documentId: "document-forecast",
    name: "Account forecast and renewal risk.xlsx",
    mediaType:
      "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  },
  {
    documentId: "document-plan",
    name: "Customer success operating plan.docx",
    mediaType:
      "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  },
  {
    documentId: "document-review",
    name: "Executive renewal review.pptx",
    mediaType:
      "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  },
  {
    documentId: "document-notes",
    name: "Interview notes.md",
    mediaType: "text/markdown",
  },
  {
    documentId: "document-data",
    name: "account-signals.json",
    mediaType: "application/json",
  },
];

function AttachmentStory({
  attachments,
  navigation,
}: {
  attachments: TranscriptFileAttachment[];
  navigation: boolean;
}) {
  const content = (
    <div className="max-w-2xl rounded-xl bg-muted p-3">
      <TranscriptFileAttachments files={attachments} />
    </div>
  );
  return navigation ? (
    <SourceNavProvider value={{ openCitation: fn(), openDocument: fn() }}>
      {content}
    </SourceNavProvider>
  ) : (
    content
  );
}

const meta = {
  title: "Attachments/Transcript files",
  component: AttachmentStory,
  args: { attachments: files.slice(0, 3), navigation: true },
} satisfies Meta<typeof AttachmentStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const CommonFormats: Story = {};

export const Dense: Story = {
  args: { attachments: files },
};

export const LongFilename: Story = {
  args: { attachments: [files[1]!] },
};

export const Compact: Story = {
  args: { attachments: files.slice(0, 4) },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const NavigationUnavailable: Story = {
  args: { attachments: files, navigation: false },
};
