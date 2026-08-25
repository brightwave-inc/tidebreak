import type { Meta, StoryObj } from "@storybook/react-vite";

import {
  DocumentViewerShell,
  DocumentViewerState,
} from "@/document/ViewerPrimitives";

type ViewerState = "loading" | "error" | "message";

function DocumentViewerStateStory({ state }: { state: ViewerState }) {
  const copy = {
    loading: "Loading document…",
    error: "This document could not be loaded.",
    message: "No preview is available for this file type.",
  }[state];

  return (
    <DocumentViewerShell className="h-[420px] rounded-lg border bg-page-background">
      <DocumentViewerState variant={state}>{copy}</DocumentViewerState>
    </DocumentViewerShell>
  );
}

const meta = {
  title: "Documents/Viewer states",
  component: DocumentViewerStateStory,
  args: { state: "loading" },
  argTypes: {
    state: {
      control: "select",
      options: ["loading", "error", "message"],
    },
  },
} satisfies Meta<typeof DocumentViewerStateStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {};

export const Error: Story = {
  args: { state: "error" },
};

export const NoPreview: Story = {
  args: { state: "message" },
};
