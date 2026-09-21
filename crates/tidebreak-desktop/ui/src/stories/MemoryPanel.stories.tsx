import { userEvent, within } from "storybook/test";
import type { Meta, StoryObj } from "@storybook/react-vite";

import type { ApiClient, MemoryRecord, MemorySettings } from "@/api";
import { MemoryPanel } from "@/settings/MemoryPanel";
import {
  memoryActive as note,
  memoryProposal as learnedThisWeek,
} from "./fixtures";

/** A preference the model saved from a conversation. */
const tables: MemoryRecord = {
  ...learnedThisWeek,
  id: "3f19d0d5-8f46-4f57-a35a-000000000011",
  kind: "preference",
  status: "active",
  title: "When formatting reports",
  body: "Use tables rather than prose for numeric comparisons.",
  observation_count: 1,
};

/** A preference the person wrote by hand. */
const terse: MemoryRecord = {
  ...tables,
  id: "3f19d0d5-8f46-4f57-a35a-000000000012",
  title: "When replying",
  body: "Keep it short. Lead with the answer.",
  provenance: {
    author: "user",
    origin: { chat_id: null, turn_id: null, code_session_id: null },
    evidence: [],
  },
};

/** A fact about the person's environment. */
const pnpm: MemoryRecord = {
  ...note,
  id: "3f19d0d5-8f46-4f57-a35a-000000000013",
  kind: "fact",
  title: "When running JavaScript tooling",
  body: "This machine uses pnpm 10; never run npm or yarn.",
  provenance: {
    ...note.provenance,
    origin: learnedThisWeek.provenance.origin,
  },
};

function digestFor(records: MemoryRecord[], byteCap = 8192) {
  const markdown = records
    .map((record) => `- ${record.updated_at.slice(0, 10)} — ${record.title}`)
    .join("\n");
  return {
    scope: { kind: "personal" },
    markdown,
    byte_len: new TextEncoder().encode(markdown).length,
    byte_cap: byteCap,
    record_count: records.length,
  };
}

const on: MemorySettings = {
  enabled: true,
  capture_enabled: true,
  capture_ready: true,
};

const off: MemorySettings = {
  enabled: false,
  capture_enabled: false,
  capture_ready: false,
};

const settingsWith = (memory: MemorySettings) =>
  ({ memory }) as unknown as Awaited<ReturnType<ApiClient["getSettings"]>>;

function stubClient(
  records: MemoryRecord[],
  options?: { fail?: boolean; memory?: MemorySettings; byteCap?: number },
): ApiClient {
  let memory: MemorySettings = options?.memory ?? on;
  let rows = records;
  return {
    getSettings: async () => {
      if (options?.fail) throw new Error("The memory backend is unavailable.");
      return settingsWith(memory);
    },
    putSettings: async (body: { memory?: Partial<MemorySettings> }) => {
      const enabled = body.memory?.enabled ?? memory.enabled;
      memory = { enabled, capture_enabled: enabled, capture_ready: enabled };
      return settingsWith(memory);
    },
    listMemoryRecords: async () => {
      if (options?.fail) throw new Error("The memory backend is unavailable.");
      return rows;
    },
    getMemoryDigest: async () => {
      if (options?.fail) throw new Error("The memory backend is unavailable.");
      return digestFor(
        rows.filter((record) => record.status === "active"),
        options?.byteCap,
      );
    },
    setMemoryRecordStatus: async (id: string, body: { status: string }) => {
      rows = rows.map((record) =>
        record.id === id
          ? { ...record, status: body.status as MemoryRecord["status"] }
          : record,
      );
      return rows.find((record) => record.id === id)!;
    },
    updateMemoryRecord: async (
      id: string,
      body: { title: string; body: string },
    ) => {
      rows = rows.map((record) =>
        record.id === id
          ? {
              ...record,
              title: body.title,
              body: body.body,
              revision: record.revision + 1,
            }
          : record,
      );
      return rows.find((record) => record.id === id)!;
    },
  } as unknown as ApiClient;
}

const meta = {
  title: "Settings/Memory",
  component: MemoryPanel,
  parameters: { layout: "fullscreen" },
  args: {
    client: stubClient([tables, terse, pnpm, note]),
    onOpenModels: () => {},
    onOpenConversation: () => {},
  },
} satisfies Meta<typeof MemoryPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** What Tidebreak knows, in the two groups the page uses. */
export const Knows: Story = {};

/** The install default: memory has not been turned on. */
export const MemoryOff: Story = {
  args: { client: stubClient([], { memory: off }) },
};

/** Just switched on: nothing learned yet, so the page says what happens next. */
export const NothingYet: Story = {
  args: { client: stubClient([]) },
};

/**
 * No configured provider serves a utility model. In-conversation saves still
 * work; the end-of-turn review does not, and the page says so.
 */
export const NoUtilityModel: Story = {
  args: {
    client: stubClient([tables, pnpm], {
      memory: { enabled: true, capture_enabled: true, capture_ready: false },
    }),
  },
};

/** The digest is past 80% of its cap. */
export const NearlyFull: Story = {
  args: {
    client: stubClient([tables, terse, pnpm, note], {
      byteCap: digestFor([tables, terse, pnpm, note]).byte_len + 8,
    }),
  },
};

/** One line open for editing. */
export const Editing: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const edits = await canvas.findAllByRole("button", { name: "Edit" });
    await userEvent.click(edits[0]);
  },
};

/** The backend read failed; retry is the only action. */
export const LoadFailed: Story = {
  args: { client: stubClient([], { fail: true }) },
};
