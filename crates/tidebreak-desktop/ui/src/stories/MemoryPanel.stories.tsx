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
  let memory: MemorySettings = options?.memory ?? {
    enabled: true,
    capture_enabled: false,
    capture_ready: false,
  };
  return {
    getSettings: async () => {
      if (options?.fail) throw new Error("The memory backend is unavailable.");
      return settingsWith(memory);
    },
    putSettings: async (body: { memory?: Partial<MemorySettings> }) => {
      memory = { ...memory, ...body.memory };
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
  title: "Settings/Memory",
  component: MemoryPanel,
  parameters: { layout: "fullscreen" },
  args: { client: stubClient([proposal, active, hypothesis], { revisions }) },
} satisfies Meta<typeof MemoryPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** One model proposal waiting for review beside active and tracked records. */
export const ReviewQueue: Story = {};

/** A first-run install: no records and no digest. */
export const Empty: Story = {
  args: { client: stubClient([], { revisions: [] }) },
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
};

/** The backend read failed; retry is the only action. */
export const LoadFailed: Story = {
  args: { client: stubClient([], { fail: true, revisions: [] }) },
};

/** A digest near its byte budget, so the meter reads as a real limit. */
export const DigestNearCap: Story = {
  args: {
    client: {
      getSettings: async () =>
        settingsWith({
          enabled: true,
          capture_enabled: true,
          capture_ready: true,
        }),
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
      sweep: sweptWithoutModel,
    }),
  },
};
