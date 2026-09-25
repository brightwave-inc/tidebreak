import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { FloatingNotices } from "@/FloatingNotices";
import { UpdateReadyCard } from "@/UpdateReadyCard";

const CHECK_FAILED =
  "Could not check for updates. Tidebreak could not reach the update server. Check your internet connection and try again.";

function UpdateReadyCardStory({
  status,
  version,
  error = null,
}: {
  status:
    | "checking"
    | "downloading"
    | "available"
    | "up-to-date"
    | "failed"
    | "ready";
  version: string | null;
  /** Why the last download, or the last restart, did not happen. */
  error?: string | null;
}) {
  const [visible, setVisible] = useState(true);
  const dismiss = () => setVisible(false);
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
      <FloatingNotices>
        {visible && status === "ready" && (
          <UpdateReadyCard
            version={version}
            error={error}
            onRestart={fn()}
            onDismiss={dismiss}
          />
        )}
        {visible && status === "available" && (
          <UpdateReadyCard
            status="available"
            version={version}
            error={error}
            onDownload={fn()}
            onDismiss={dismiss}
          />
        )}
        {visible && status === "up-to-date" && (
          <UpdateReadyCard
            status="up-to-date"
            version={version}
            onDismiss={dismiss}
          />
        )}
        {visible && status === "failed" && (
          <UpdateReadyCard
            status="failed"
            message={CHECK_FAILED}
            onRetry={fn()}
            onDismiss={dismiss}
          />
        )}
        {visible && (status === "checking" || status === "downloading") && (
          <UpdateReadyCard
            status={status}
            version={version}
            onDismiss={dismiss}
          />
        )}
      </FloatingNotices>
    </div>
  );
}

const meta = {
  title: "Shell/Update card",
  component: UpdateReadyCardStory,
  parameters: { layout: "fullscreen" },
  args: { status: "ready", version: "0.59.0" },
} satisfies Meta<typeof UpdateReadyCardStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Ready: Story = {};

export const Checking: Story = {
  args: { status: "checking", version: null },
};

export const Downloading: Story = {
  args: { status: "downloading", version: "0.59.0" },
};

export const VersionUnavailable: Story = {
  args: { version: null },
};

/** Automatic downloads are off, so the release waits for you to download it. */
export const Available: Story = {
  args: { status: "available", version: "0.115.0" },
};

/** The check you asked for found nothing newer than the version you run. */
export const UpToDate: Story = {
  args: { status: "up-to-date", version: "0.114.0" },
};

/** The check you asked for failed, and the card says why. */
export const CheckFailed: Story = {
  args: { status: "failed", version: null },
};

/** The download could not be saved, so the card offers it again with why. */
export const AvailableDownloadFailed: Story = {
  args: {
    status: "available",
    version: "0.115.0",
    error:
      "Not enough disk space to download the update. Free up space, then try again.",
  },
};

/**
 * Restart and update was refused: a code turn was still running when the
 * restart's wait ran out. The card keeps the desktop's reason and the action
 * it needs, and still offers the restart.
 */
export const RestartRefused: Story = {
  args: {
    status: "ready",
    version: "0.117.0",
    error:
      "A code session is still working on a turn. Stop the running turn, or let it finish, then restart. The update stays ready.",
  },
};
