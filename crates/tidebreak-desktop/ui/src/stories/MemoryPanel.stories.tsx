import { userEvent, within } from "storybook/test";
import type { Meta, StoryObj } from "@storybook/react-vite";

import type {
  ApiClient,
  MemoryRecord,
  MemoryRevision,
  MemorySettings,
  MemorySweepStatus,
} from "@/api";
import { MemoryPanel } from "@/settings/MemoryPanel";
import {
  memoryActive as active,
  memoryProposal as proposal,
  memoryTracking as hypothesis,
} from "./fixtures";

const revisions: MemoryRevision[] = [
  {
    id: "7f586e60-0000-4000-8000-000000000001",
    record_id: proposal.id,
    ordinal: 1,
    snapshot: proposal,
    created_at: proposal.created_at,
  },
];

/** A second watched pattern, seen more than once in the same conversation. */
const repeated: MemoryRecord = {
  ...hypothesis,
  id: "3f19d0d5-8f46-4f57-a35a-000000000004",
  title: "When asked for a status update",
  body: "Lead with the blocker, then the plan.",
  observation_count: 3,
  created_at: "2026-08-28T14:00:00Z",
  updated_at: "2026-09-02T09:15:00Z",
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

const neverRan: MemorySweepStatus = { last_run: null };

const sweptWithProposal: MemorySweepStatus = {
  last_run: {
    ran_at: "2026-09-02T08:30:00Z",
    scope: { kind: "personal" },
    outcome: "proposed",
    expired: 1,
    proposed: 1,
  },
};

const sweptParked: MemorySweepStatus = {
  last_run: {
    ran_at: "2026-09-02T08:30:00Z",
    scope: { kind: "personal" },
    outcome: "parked",
    expired: 0,
    proposed: 0,
  },
};

const sweptWithoutModel: MemorySweepStatus = {
  last_run: {
    ran_at: "2026-09-02T08:30:00Z",
    scope: { kind: "personal" },
    outcome: "no_model",
    expired: 0,
    proposed: 0,
  },
};

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
  options?: {
    fail?: boolean;
    revisions?: MemoryRevision[];
    memory?: MemorySettings;
    sweep?: MemorySweepStatus;
  },
): ApiClient {
  let memory: MemorySettings = options?.memory ?? on;
  return {
    getSettings: async () => {
      if (options?.fail) throw new Error("The memory backend is unavailable.");
      return settingsWith(memory);
    },
    putSettings: async (body: { memory?: Partial<MemorySettings> }) => {
      const enabled = body.memory?.enabled ?? memory.enabled;
      const capture = enabled && (body.memory?.capture_enabled ?? true);
      memory = { enabled, capture_enabled: capture, capture_ready: capture };
      return settingsWith(memory);
    },
    setMemoryRecordStatus: async () => records[0],
    deleteMemoryRecord: async () => undefined,
    listMemoryRecords: async () => {
      if (options?.fail) throw new Error("The memory backend is unavailable.");
      return records;
    },
    getMemoryDigest: async () => {
      if (options?.fail) throw new Error("The memory backend is unavailable.");
      return digestFor(records.filter((record) => record.status === "active"));
    },
    getMemorySweepStatus: async () => {
      if (options?.fail) throw new Error("The memory backend is unavailable.");
      return options?.sweep ?? neverRan;
    },
    getMemoryRevisions: async () => options?.revisions ?? [],
  } as unknown as ApiClient;
}

const meta = {
  title: "Settings/Experimental",
  component: MemoryPanel,
  parameters: { layout: "fullscreen" },
  args: {
    client: stubClient([proposal, active, hypothesis], { revisions }),
    onOpenModels: () => {},
  },
} satisfies Meta<typeof MemoryPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** One model proposal waiting for review beside active and watched records. */
export const ReviewQueue: Story = {};

/** The install default: memory has not been turned on. Records persist. */
export const MemoryDisabled: Story = {
  args: {
    client: stubClient([], { revisions: [], memory: off }),
  },
};

/** Just switched on: nothing captured yet, so the page says what happens next. */
export const JustEnabled: Story = {
  args: { client: stubClient([], { revisions: [] }) },
};

/**
 * Memory is on but no configured provider serves a utility model, so capture
 * cannot run. The status names the fix and offers the way to Models.
 */
export const NoUtilityModel: Story = {
  args: {
    client: stubClient([], {
      revisions: [],
      memory: { enabled: true, capture_enabled: true, capture_ready: false },
    }),
  },
};

/** Capture was paused through the API; injection continues and a resume is one click. */
export const CapturePaused: Story = {
  args: {
    client: stubClient([active], {
      revisions: [],
      memory: { enabled: true, capture_enabled: false, capture_ready: false },
    }),
  },
};

/**
 * Capture has seen two patterns but neither has repeated in another
 * conversation yet. The Review view is empty and points at what is being
 * watched, so the feature reads as working rather than broken.
 */
export const WatchingOnly: Story = {
  args: {
    client: stubClient([hypothesis, repeated], { revisions: [] }),
  },
};

/** The Noticing view itself, with a watched record selected. */
export const Noticing: Story = {
  args: {
    client: stubClient([hypothesis, repeated], { revisions: [] }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: /^Noticing/ }),
    );
    await userEvent.click(
      await canvas.findByRole("button", { name: /When asked for a status/ }),
    );
  },
};

/** An active record with its provenance and revision history. */
export const ActiveRecord: Story = {
  args: {
    client: stubClient([active, proposal], {
      revisions: [
        {
          id: "7f586e60-0000-4000-8000-000000000002",
          record_id: active.id,
          ordinal: 2,
          snapshot: active,
          created_at: active.updated_at,
        },
      ],
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: /^All records/ }),
    );
    await userEvent.click(
      await canvas.findByRole("button", { name: /When preparing a release/ }),
    );
  },
};

/** The backend read failed; retry is the only action. */
export const LoadFailed: Story = {
  args: { client: stubClient([], { fail: true, revisions: [] }) },
};

/** A digest near its byte budget, so the meter reads as a real limit. */
export const DigestNearCap: Story = {
  args: {
    client: {
      getSettings: async () => settingsWith(on),
      listMemoryRecords: async () => [active, proposal],
      getMemoryDigest: async () =>
        digestFor([active], Math.max(1, digestFor([active]).byte_len - 1)),
      getMemorySweepStatus: async () => neverRan,
      getMemoryRevisions: async () => revisions,
    } as unknown as ApiClient,
  },
};

/** Maintenance archived an expired record and proposed a merge for review. */
export const MaintenanceProposed: Story = {
  args: {
    client: stubClient([proposal, active, hypothesis], {
      revisions,
      sweep: sweptWithProposal,
    }),
  },
};

/** A dismissed merge parked the scope until its records change. */
export const MaintenanceParked: Story = {
  args: {
    client: stubClient([active, hypothesis], {
      revisions: [],
      sweep: sweptParked,
    }),
  },
};

/** No utility model resolves, so only mechanical expiry runs. */
export const MaintenanceNoModel: Story = {
  args: {
    client: stubClient([active, hypothesis], {
      revisions: [],
      memory: { enabled: true, capture_enabled: true, capture_ready: false },
      sweep: sweptWithoutModel,
    }),
  },
};
