// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { MemoryRecord, MemorySettings } from "@/api";
import { MemoryPanel } from "./MemoryPanel";

const tracking: MemoryRecord = {
  id: "3f19d0d5-8f46-4f57-a35a-000000000003",
  scope: { kind: "personal" },
  kind: "preference",
  status: "tracking",
  title: "When reviewing pull requests",
  body: "Lead with the risk, then the diff.",
  provenance: {
    author: "model",
    origin: {
      chat_id: "9d5d84a0-6ba6-4c73-9e10-000000000001",
      turn_id: "turn-1",
      code_session_id: null,
    },
    evidence: [
      { kind: "message", message_id: "c6a0d000-0000-4000-8000-000000000002" },
    ],
  },
  links: [],
  expires_at: null,
  superseded_by: null,
  observation_count: 2,
  revision: 1,
  created_at: "2026-09-01T09:00:00Z",
  updated_at: "2026-09-01T09:00:00Z",
};

const off: MemorySettings = {
  enabled: false,
  capture_enabled: false,
  capture_ready: false,
};

function stubClient(records: MemoryRecord[], memory: MemorySettings) {
  const putSettings = vi.fn(
    async (body: { memory?: Partial<MemorySettings> }) => {
      const enabled = body.memory?.enabled ?? memory.enabled;
      const capture = enabled && (body.memory?.capture_enabled ?? true);
      return {
        memory: { enabled, capture_enabled: capture, capture_ready: capture },
      };
    },
  );
  const setMemoryRecordStatus = vi.fn(
    async (id: string, body: { status: MemoryRecord["status"] }) => ({
      ...records.find((record) => record.id === id)!,
      status: body.status,
      revision: 2,
    }),
  );
  return {
    client: {
      getSettings: async () => ({ memory }),
      putSettings,
      listMemoryRecords: async () => records,
      getMemoryDigest: async () => ({
        scope: { kind: "personal" },
        markdown: "",
        byte_len: 0,
        byte_cap: 8192,
        record_count: 0,
      }),
      getMemorySweepStatus: async () => ({ last_run: null }),
      getMemoryRevisions: async () => [],
      setMemoryRecordStatus,
    } as never,
    putSettings,
    setMemoryRecordStatus,
  };
}

afterEach(cleanup);

describe("MemoryPanel", () => {
  it("turns capture on with the one memory switch", async () => {
    const { client, putSettings } = stubClient([], off);
    render(<MemoryPanel client={client} />);
    expect(await screen.findByText("Memory is off")).toBeInTheDocument();
    const toggle = screen.getByRole("switch", {
      name: /^Remember across conversations/,
    });
    expect(toggle).not.toBeChecked();
    await userEvent.click(toggle);
    expect(putSettings).toHaveBeenCalledWith({
      memory: { enabled: true, capture_enabled: true },
    });
    await waitFor(() => expect(toggle).toBeChecked());
    expect(screen.getByText("Memory is on")).toBeInTheDocument();
  });

  it("names the missing utility model and offers the way to Models", async () => {
    const onOpenModels = vi.fn();
    const { client } = stubClient([], {
      enabled: true,
      capture_enabled: true,
      capture_ready: false,
    });
    render(<MemoryPanel client={client} onOpenModels={onOpenModels} />);
    expect(
      await screen.findByText("Memory is on, but nothing can be captured yet"),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Open Models" }));
    expect(onOpenModels).toHaveBeenCalledOnce();
  });

  it("resumes a paused capture without touching the memory switch", async () => {
    const { client, putSettings } = stubClient([], {
      enabled: true,
      capture_enabled: false,
      capture_ready: false,
    });
    render(<MemoryPanel client={client} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Resume capture" }),
    );
    expect(putSettings).toHaveBeenCalledWith({
      memory: { capture_enabled: true },
    });
  });

  it("shows watched patterns with their sightings and sends one to review", async () => {
    const { client, setMemoryRecordStatus } = stubClient([tracking], {
      enabled: true,
      capture_enabled: true,
      capture_ready: true,
    });
    render(<MemoryPanel client={client} />);
    // The status names what is being watched, and the page opens on the
    // Noticing view because that is where the only records are.
    expect(
      await screen.findByText(/1 pattern is being watched/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Noticing/ })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByText(/Seen 2 times/)).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: /When reviewing pull requests/ }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Review now" }));
    expect(setMemoryRecordStatus).toHaveBeenCalledWith(tracking.id, {
      expected_revision: 1,
      status: "proposed",
    });
  });
});
