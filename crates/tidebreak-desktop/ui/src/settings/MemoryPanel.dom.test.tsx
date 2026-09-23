// @vitest-environment jsdom

import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { MemoryRecord, MemorySettings } from "@/api";
import { useMemoryPresenceStore } from "../MemoryPresenceStore";
import { MemoryPanel } from "./MemoryPanel";

const preference: MemoryRecord = {
  id: "3f19d0d5-8f46-4f57-a35a-000000000003",
  scope: { kind: "personal" },
  kind: "preference",
  status: "active",
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
  observation_count: 1,
  revision: 1,
  created_at: "2026-09-01T09:00:00Z",
  updated_at: "2026-09-01T09:00:00Z",
};

const fact: MemoryRecord = {
  ...preference,
  id: "3f19d0d5-8f46-4f57-a35a-000000000004",
  kind: "fact",
  title: "When running JavaScript tooling",
  body: "This machine uses pnpm.",
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

function stubClient(records: MemoryRecord[], memory: MemorySettings) {
  const putSettings = vi.fn(
    async (body: { memory?: Partial<MemorySettings> }) => {
      const enabled = body.memory?.enabled ?? memory.enabled;
      return {
        memory: { enabled, capture_enabled: enabled, capture_ready: enabled },
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
  const updateMemoryRecord = vi.fn(
    async (id: string, body: { title: string; body: string }) => ({
      ...records.find((record) => record.id === id)!,
      title: body.title,
      body: body.body,
      revision: 2,
    }),
  );
  const deleteMemoryRecord = vi.fn(async (_id: string) => undefined);
  const deleteAllMemoryRecords = vi.fn(async () => ({
    deleted: records.length,
  }));
  return {
    client: {
      getSettings: async () => ({ memory }),
      putSettings,
      listMemoryRecords: async () => records,
      deleteMemoryRecord,
      deleteAllMemoryRecords,
      getMemoryDigest: async () => ({
        scope: { kind: "personal" },
        markdown: "",
        byte_len: 0,
        byte_cap: 8192,
        record_count: records.filter((record) => record.status === "active")
          .length,
      }),
      setMemoryRecordStatus,
      updateMemoryRecord,
    } as never,
    putSettings,
    setMemoryRecordStatus,
    updateMemoryRecord,
    deleteMemoryRecord,
    deleteAllMemoryRecords,
  };
}

const forgotten: MemoryRecord = {
  ...fact,
  id: "3f19d0d5-8f46-4f57-a35a-000000000005",
  status: "archived",
  title: "When naming branches",
  body: "Prefix them with the ticket number.",
  revision: 2,
};

afterEach(() => {
  cleanup();
  useMemoryPresenceStore.getState().reset();
});

describe("MemoryPanel", () => {
  it("turns memory on with the one switch", async () => {
    const { client, putSettings } = stubClient([], off);
    render(<MemoryPanel client={client} />);
    expect(await screen.findByText("Memory is off")).toBeInTheDocument();
    const toggle = screen.getByRole("switch", {
      name: /^Remember across conversations/,
    });
    await userEvent.click(toggle);
    expect(putSettings).toHaveBeenCalledWith({
      memory: { enabled: true, capture_enabled: true },
    });
    await waitFor(() => expect(toggle).toBeChecked());
  });

  it("groups what it knows, opens the source conversation, and forgets a line", async () => {
    const onOpenConversation = vi.fn();
    const { client, setMemoryRecordStatus } = stubClient(
      [preference, fact],
      on,
    );
    render(
      <MemoryPanel client={client} onOpenConversation={onOpenConversation} />,
    );
    expect(await screen.findByText("About you")).toBeInTheDocument();
    expect(screen.getByText("Notes")).toBeInTheDocument();
    expect(
      screen.getByText("When reviewing pull requests"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("When running JavaScript tooling"),
    ).toBeInTheDocument();
    // No lifecycle vocabulary reaches the page.
    expect(screen.queryByText(/revision/)).not.toBeInTheDocument();
    expect(screen.queryByText(/^active$/)).not.toBeInTheDocument();

    const [openConversation] = screen.getAllByRole("button", {
      name: "Open conversation",
    });
    await userEvent.click(openConversation);
    expect(onOpenConversation).toHaveBeenCalledWith(
      preference.provenance.origin.chat_id,
    );

    const [forget] = screen.getAllByRole("button", { name: "Forget" });
    await userEvent.click(forget);
    expect(setMemoryRecordStatus).toHaveBeenCalledWith(preference.id, {
      expected_revision: 1,
      status: "archived",
    });
  });

  it("saves an edit as the person's own words", async () => {
    const { client, updateMemoryRecord } = stubClient([preference], on);
    render(<MemoryPanel client={client} />);
    await userEvent.click(await screen.findByRole("button", { name: "Edit" }));
    const body = screen.getByLabelText("Memory body");
    await userEvent.clear(body);
    await userEvent.type(body, "Lead with the risk.");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(updateMemoryRecord).toHaveBeenCalledWith(
      preference.id,
      expect.objectContaining({
        expected_revision: 1,
        body: "Lead with the risk.",
        author: "user",
      }),
    );
  });

  it("deletes a line for good, behind a confirmation, rather than archiving it", async () => {
    const { client, deleteMemoryRecord, setMemoryRecordStatus } = stubClient(
      [preference],
      on,
    );
    render(<MemoryPanel client={client} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Delete" }),
    );
    expect(
      await screen.findByRole("heading", { name: "Delete this memory?" }),
    ).toBeInTheDocument();
    expect(deleteMemoryRecord).not.toHaveBeenCalled();

    const dialog = screen.getByRole("alertdialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Delete" }),
    );
    await waitFor(() =>
      expect(deleteMemoryRecord).toHaveBeenCalledWith(preference.id),
    );
    expect(setMemoryRecordStatus).not.toHaveBeenCalled();
  });

  it("keeps forgotten lines collapsed, and restores or deletes one", async () => {
    const { client, deleteMemoryRecord, setMemoryRecordStatus } = stubClient(
      [preference, forgotten],
      on,
    );
    render(<MemoryPanel client={client} />);
    const toggle = await screen.findByRole("button", {
      name: "Show 1 forgotten record",
    });
    expect(screen.queryByText("When naming branches")).not.toBeInTheDocument();

    await userEvent.click(toggle);
    expect(screen.getByText("When naming branches")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Restore" }));
    expect(setMemoryRecordStatus).toHaveBeenCalledWith(forgotten.id, {
      expected_revision: 2,
      status: "active",
    });

    const deletes = screen.getAllByRole("button", { name: "Delete" });
    await userEvent.click(deletes[deletes.length - 1]);
    const dialog = await screen.findByRole("alertdialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Delete" }),
    );
    await waitFor(() =>
      expect(deleteMemoryRecord).toHaveBeenCalledWith(forgotten.id),
    );
  });

  it("deletes everything, forgotten lines included, from the danger zone", async () => {
    const { client, deleteAllMemoryRecords } = stubClient(
      [preference, forgotten],
      on,
    );
    render(<MemoryPanel client={client} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Delete everything" }),
    );
    const dialog = await screen.findByRole("alertdialog");
    expect(
      within(dialog).getByText(/all 2 records, forgotten ones included/),
    ).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Delete everything" }),
    );
    await waitFor(() => expect(deleteAllMemoryRecords).toHaveBeenCalledOnce());
  });

  it("names the missing utility model without hiding what it knows", async () => {
    const onOpenModels = vi.fn();
    const { client } = stubClient([fact], {
      enabled: true,
      capture_enabled: true,
      capture_ready: false,
    });
    render(<MemoryPanel client={client} onOpenModels={onOpenModels} />);
    expect(
      await screen.findByText("Memory saves only during conversations"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("When running JavaScript tooling"),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Open Models" }));
    expect(onOpenModels).toHaveBeenCalledOnce();
  });
});
