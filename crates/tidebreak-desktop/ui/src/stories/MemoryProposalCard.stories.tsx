import type { Meta, StoryObj } from "@storybook/react-vite";

import type { ApiClient, MemoryRecord } from "@/api";
import { MemoryProposalCard } from "@/MemoryProposalCard";
import { memoryActive, memoryProposal } from "./fixtures";

const second: MemoryRecord = {
  ...memoryProposal,
  id: "3f19d0d5-8f46-4f57-a35a-000000000011",
  kind: "preference",
  title: "When drafting release notes",
  body: "Lead with the user-visible change, not the module name.",
};

const third: MemoryRecord = {
  ...memoryProposal,
  id: "3f19d0d5-8f46-4f57-a35a-000000000012",
  kind: "fact",
  title: "When deploying the staging stack",
  body: "The staging database lives in the eu-west region.",
};

const rejected: MemoryRecord = {
  ...second,
  status: "rejected",
  revision: 2,
};

/**
 * A stub client that answers every mutation with the state the server would
 * return, revision bumped, so the card's optimistic replacement is visible.
 */
const client = {
  setMemoryRecordStatus: async (recordId, body) => {
    const record = [memoryProposal, second, third].find(
      (candidate) => candidate.id === recordId,
    );
    if (!record) throw new Error("unknown record");
    return {
      ...record,
      status: body.status,
      revision: body.expected_revision + 1,
    };
  },
  updateMemoryRecord: async (recordId, body) => ({
    ...memoryProposal,
    id: recordId,
    kind: body.kind,
    title: body.title,
    body: body.body,
    revision: body.expected_revision + 1,
  }),
} satisfies Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">;

/** A stub client whose every mutation fails, for the inline error line. */
const failingClient = {
  setMemoryRecordStatus: async () => {
    throw new Error("The record changed under you; reload and retry.");
  },
  updateMemoryRecord: async () => {
    throw new Error("The record changed under you; reload and retry.");
  },
} satisfies Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">;

const meta = {
  title: "Conversation/Memory proposals",
  component: MemoryProposalCard,
  args: {
    turnId: "turn-storybook-memory",
    records: [memoryProposal],
    client,
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-3xl p-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof MemoryProposalCard>;

export default meta;
type Story = StoryObj<typeof meta>;

/** One pending proposal: singular title and a warning badge of one. */
export const OneProposal: Story = {};

/** Three pending proposals of different kinds, each with its own actions. */
export const SeveralProposals: Story = {
  args: { records: [memoryProposal, second, third] },
};

/** Every proposal decided: reviewed title, status badges, no actions. */
export const AllDecided: Story = {
  args: {
    records: [
      { ...memoryActive, revision: 2 },
      rejected,
      { ...third, status: "archived", revision: 2 },
    ],
  },
};

/** A proposal already approved into the digest beside one still pending. */
export const Approved: Story = {
  args: {
    records: [{ ...memoryProposal, status: "active", revision: 2 }, second],
  },
};

/** A proposal the reader dismissed, shown as a critical badge. */
export const Dismissed: Story = {
  args: { records: [rejected] },
};

/**
 * Live editing against the stub client: expand the row, choose Edit, change
 * the title or body, and Save replaces the record with the server's answer.
 */
export const Editing: Story = {
  args: { records: [memoryProposal, second] },
};

/** Every decision and save fails, leaving the row with its inline error. */
export const MutationFails: Story = {
  args: { client: failingClient },
};
