import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, within } from "storybook/test";

import type { ApiClient } from "@/api/client";
import { DiffPanel } from "@/code/DiffPanel";

type DiffScenario =
  | "live"
  | "retained"
  | "host"
  | "loading"
  | "empty"
  | "unavailable";

/** Long enough that a narrow header has to choose between path and chip. */
const PATH =
  "crates/tidebreak-server/src/code/remote/checkpoints/periodic_push.rs";

const DIFF = [
  `diff --git a/${PATH} b/${PATH}`,
  `--- a/${PATH}`,
  `+++ b/${PATH}`,
  "@@ -1,4 +1,6 @@",
  " /// Pushed about once a minute while a turn runs.",
  "-pub const PERIODIC_SECS: u64 = 90;",
  "+pub const PERIODIC_SECS: u64 = 60;",
  "+",
  "+/// The terminal push always runs, even after a failed turn.",
  ' pub const TERMINAL: &str = "terminal";',
].join("\n");

function clientFor(
  scenario: DiffScenario,
): Pick<ApiClient, "getCodeWorkspaceDiff"> {
  return {
    getCodeWorkspaceDiff: async () => {
      if (scenario === "loading") return new Promise<never>(() => {});
      if (scenario === "unavailable") {
        throw new Error(
          "The sandbox has not saved a checkpoint yet. Its first live checkpoint lands within about a minute of the turn starting. Open the transcript meanwhile.",
        );
      }
      const empty = scenario === "empty";
      return {
        diff: empty ? "" : DIFF,
        truncated: false,
        stat: empty
          ? { files: 0, insertions: 0, deletions: 0, truncated: false }
          : { files: 1, insertions: 3, deletions: 1, truncated: false },
        file: PATH,
        ...(scenario === "live"
          ? {
              revision: "live" as const,
              revision_ref: "mg-wip/sb-1-i2",
              revision_saved_at: new Date(Date.now() - 42_000).toISOString(),
            }
          : scenario === "retained" || scenario === "empty"
            ? {
                revision: "retained" as const,
                revision_ref: "mg-wip/sb-1-i1",
                revision_saved_at: "2026-09-21T18:23:00.000Z",
              }
            : {}),
      };
    },
  };
}

function DiffDetailStory({
  scenario,
  width,
}: {
  scenario: DiffScenario;
  width?: number;
}) {
  return (
    <div
      className="flex h-[420px] min-h-0 flex-col overflow-hidden rounded-lg border bg-background"
      style={width ? { width } : undefined}
    >
      <DiffPanel
        client={clientFor(scenario)}
        workspaceId="workspace-storybook"
        file={PATH}
        onOpenFile={() => {}}
      />
    </div>
  );
}

/**
 * The standalone diff detail for one file. A remote workspace's diff names
 * the same checkpoint chip Files shows, so the two panes never disagree
 * silently about which revision they read.
 */
const meta = {
  title: "Code/Diff detail",
  component: DiffDetailStory,
  args: { scenario: "live" },
  argTypes: {
    scenario: {
      control: "select",
      options: ["live", "retained", "host", "loading", "empty", "unavailable"],
    },
  },
} satisfies Meta<typeof DiffDetailStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const LiveCheckpoint: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.findByText(/^Live · saved/)).resolves.toBeVisible();
  },
};

export const RetainedCheckpoint: Story = {
  args: { scenario: "retained" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.findByText("Saved checkpoint")).resolves.toBeVisible();
  },
};

/** A host worktree reads its own files, so it carries no checkpoint chip. */
export const HostWorktree: Story = {
  args: { scenario: "host" },
};

export const Loading: Story = {
  args: { scenario: "loading" },
};

export const NoChanges: Story = {
  args: { scenario: "empty" },
};

export const SandboxUnavailable: Story = {
  args: { scenario: "unavailable" },
};

/** The path keeps its row; the chip and actions wrap under it. */
export const LiveCheckpointNarrow: Story = {
  args: { scenario: "live", width: 358 },
};

export const RetainedCheckpointNarrow: Story = {
  args: { scenario: "retained", width: 358 },
};
