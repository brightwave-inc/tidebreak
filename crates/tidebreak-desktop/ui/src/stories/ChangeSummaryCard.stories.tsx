import type { Meta, StoryObj } from "@storybook/react-vite";

import type { ApiClient, ExecFileChangeSummary } from "@/api";
import { ChangeSummaryCard } from "@/ChangeSummaryCard";

const files: ExecFileChangeSummary[] = [
  {
    snapshot_id: "snapshot-readme",
    folder_name: "Tidebreak",
    relative_path: "README.md",
    classification: "applied",
    change: "overwritten",
    rejection_reason: null,
    undo: "available",
    diff: "--- before\n+++ after\n@@ -1 +1 @@\n-Old setup\n+Updated setup\n",
    binary_preview: null,
  },
  {
    snapshot_id: "snapshot-stale",
    folder_name: "Tidebreak",
    relative_path: "src/session.ts",
    classification: "rejected",
    change: null,
    rejection_reason: "stale",
    undo: "not_available",
    diff: null,
    binary_preview: null,
  },
];

const client = {
  getFileChangePreview: async () => new Blob(),
  undoFileChange: async (_chatId, _turnId, snapshotId) => ({
    snapshot_id: snapshotId,
    folder_name: "Tidebreak",
    relative_path: "README.md",
    status: "restored" as const,
  }),
  undoTurnFileChanges: async (chatId, turnId) => ({
    chat_id: chatId,
    turn_id: turnId,
    files: [],
  }),
} satisfies Pick<
  ApiClient,
  "getFileChangePreview" | "undoFileChange" | "undoTurnFileChanges"
>;

const meta = {
  title: "Conversation/Change summary",
  component: ChangeSummaryCard,
  args: {
    client,
    chatId: "chat-review",
    turnId: "turn-writeback",
    files,
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-3xl p-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ChangeSummaryCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const MixedResults: Story = {};

export const NothingToUndo: Story = {
  args: {
    files: files.map((file) =>
      file.classification === "applied"
        ? { ...file, undo: "already_undone" as const }
        : file,
    ),
  },
};
