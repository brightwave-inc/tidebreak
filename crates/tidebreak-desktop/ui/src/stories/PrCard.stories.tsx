import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import type { CodeWorkspacePrSnapshot } from "@/api";
import { PrCardView } from "@/code/PrCard";
import {
  cleanGit,
  dirtyGit,
  hostedAppGit,
  hostedPersonGit,
  openPrGit,
  queuedPrGit,
  readyForPrGit,
  unpushedGit,
} from "./fixtures";

function GitState({
  snapshot,
  busy = null,
  error,
}: {
  snapshot: CodeWorkspacePrSnapshot | null;
  busy?: "commit" | "push" | "create_pr" | null;
  error?: string;
}) {
  const [message, setMessage] = useState(
    snapshot?.suggested_commit_message ?? "Add the scoped Storybook workshop",
  );
  return (
    <PrCardView
      snapshot={snapshot}
      error={error}
      message={message}
      busy={busy}
      onMessageChange={setMessage}
      onCommit={fn()}
      onPush={fn()}
      onCreatePr={fn()}
      onRefresh={fn()}
    />
  );
}

const meta = {
  title: "Code/Git state",
  component: GitState,
  args: { snapshot: cleanGit },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof GitState>;

export default meta;
type Story = StoryObj<typeof meta>;

export const NoCommits: Story = {};

export const Uncommitted: Story = {
  args: { snapshot: dirtyGit },
};

export const Committing: Story = {
  args: { snapshot: dirtyGit, busy: "commit" },
};

export const Unpushed: Story = {
  args: { snapshot: unpushedGit },
};

/**
 * A gateway-authenticated hosted machine pushes with the deployment's GitHub
 * App identity, and the card says so plainly: work lands as the App's bot
 * account, not as the person (issue #2510).
 */
export const HostedPushesAsApp: Story = {
  args: { snapshot: hostedAppGit },
};

/**
 * A hosted machine whose caller connected their own GitHub account at the
 * gateway pushes as that person, and the card names them (decision 65).
 */
export const HostedPushesAsYou: Story = {
  args: { snapshot: hostedPersonGit },
};

export const ReadyForPullRequest: Story = {
  args: { snapshot: readyForPrGit },
};

export const PullRequestOpen: Story = {
  args: { snapshot: openPrGit },
};

export const MergeQueued: Story = {
  args: { snapshot: queuedPrGit },
};

export const RefreshFailed: Story = {
  args: {
    snapshot: openPrGit,
    error: "Git status could not be refreshed. The last known state is shown.",
  },
};
