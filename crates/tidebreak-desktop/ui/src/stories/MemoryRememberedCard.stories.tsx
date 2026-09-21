import { userEvent, within } from "storybook/test";
import type { Meta, StoryObj } from "@storybook/react-vite";

import type { ApiClient, MemoryRecord } from "@/api";
import { MemoryRememberedCard } from "@/MemoryRememberedCard";
import { memoryActive, memoryProposal } from "./fixtures";

const remembered: MemoryRecord = {
  ...memoryProposal,
  status: "active",
  observation_count: 1,
};

const second: MemoryRecord = {
  ...memoryActive,
  id: "3f19d0d5-8f46-4f57-a35a-000000000009",
  kind: "preference",
  title: "When drafting release notes",
  body: "Lead with what changed for users, then the fixes.",
  provenance: {
    ...memoryActive.provenance,
    origin: memoryProposal.provenance.origin,
  },
};

const client = {
  setMemoryRecordStatus: async (_id, body) => ({
    ...remembered,
    status: body.status,
    revision: remembered.revision + 1,
  }),
  updateMemoryRecord: async (_id, body) => ({
    ...remembered,
    title: body.title,
    body: body.body,
    revision: remembered.revision + 1,
  }),
} satisfies Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">;

const meta = {
  title: "Conversation/Remembered",
  component: MemoryRememberedCard,
  args: {
    turnId: "turn-storybook-memory",
    records: [remembered],
    client: client as unknown as ApiClient,
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl p-6">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof MemoryRememberedCard>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A turn that saved one thing. The row names it and offers Edit and Forget. */
export const OneThing: Story = {};

/** A turn that saved two things reads as a count until expanded. */
export const SeveralThings: Story = {
  args: { records: [remembered, second] },
};

/** The person forgot what the turn saved; the row keeps the trail. */
export const Forgotten: Story = {
  args: { records: [{ ...remembered, status: "archived" }] },
};

/** The row opened: what was kept, with Edit and Forget. */
export const Expanded: Story = {
  args: { records: [remembered, second] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: /Remembered 2 things/ }),
    );
  },
};
