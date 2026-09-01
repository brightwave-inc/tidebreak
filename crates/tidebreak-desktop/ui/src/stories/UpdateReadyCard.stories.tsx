import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { UpdateReadyCard } from "@/UpdateReadyCard";

function UpdateReadyCardStory({ version }: { version: string | null }) {
  const [visible, setVisible] = useState(true);
  return (
    <div className="h-screen bg-page-background p-8">
      <div className="mx-auto max-w-2xl rounded-xl border border-border-subtle bg-background p-8">
        <p className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Conversation
        </p>
        <h1 className="mt-2 text-2xl font-semibold tracking-tight">
          Plan the next release
        </h1>
        <p className="mt-2 max-w-lg text-sm text-muted-foreground">
          The card stays available without blocking the work underneath it.
        </p>
      </div>
      {visible && (
        <UpdateReadyCard
          version={version}
          onRestart={fn()}
          onDismiss={() => setVisible(false)}
        />
      )}
    </div>
  );
}

const meta = {
  title: "Shell/Update ready card",
  component: UpdateReadyCardStory,
  parameters: { layout: "fullscreen" },
  args: { version: "0.59.0" },
} satisfies Meta<typeof UpdateReadyCardStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Ready: Story = {};

export const VersionUnavailable: Story = {
  args: { version: null },
};
