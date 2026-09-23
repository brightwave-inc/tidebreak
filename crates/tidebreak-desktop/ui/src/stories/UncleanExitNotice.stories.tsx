import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import type { DiagnosticsSaveState } from "@/desktopLifecycle";
import { FloatingNotices } from "@/FloatingNotices";
import { UncleanExitNotice } from "@/UncleanExitNotice";
import { UpdateReadyCard } from "@/UpdateReadyCard";

function UncleanExitNoticeStory({
  save,
  withUpdate,
}: {
  save: DiagnosticsSaveState;
  withUpdate: boolean;
}) {
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
          The notice waits in the corner without blocking the work underneath
          it.
        </p>
      </div>
      <FloatingNotices>
        {visible && (
          <UncleanExitNotice
            save={save}
            onSave={fn()}
            onDismiss={() => setVisible(false)}
          />
        )}
        {withUpdate && (
          <UpdateReadyCard
            version="0.115.0"
            onRestart={fn()}
            onDismiss={fn()}
          />
        )}
      </FloatingNotices>
    </div>
  );
}

const meta = {
  title: "Shell/Unclean exit notice",
  component: UncleanExitNoticeStory,
  parameters: { layout: "fullscreen" },
  args: { save: { status: "idle" }, withUpdate: false },
} satisfies Meta<typeof UncleanExitNoticeStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The launch after a crash, a force quit, or a power loss. */
export const Offer: Story = {};

export const Saving: Story = {
  args: { save: { status: "saving" } },
};

export const Saved: Story = {
  args: { save: { status: "saved" } },
};

export const SaveFailed: Story = {
  args: {
    save: {
      status: "failed",
      error: "Could not build the diagnostics report",
    },
  },
};

/** Beside an update that is ready, the two stack instead of overlapping. */
export const WithAnUpdateReady: Story = {
  args: { withUpdate: true },
};
