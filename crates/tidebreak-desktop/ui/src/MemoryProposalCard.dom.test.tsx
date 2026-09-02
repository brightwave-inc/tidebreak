// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, MemoryRecord } from "./api";
import { MemoryProposalCard } from "./MemoryProposalCard";

afterEach(() => {
  cleanup();
});

const record: MemoryRecord = {
  id: "record-1",
  scope: { kind: "personal" },
  kind: "lesson",
  status: "proposed",
  title: "When changing database migrations",
  body: "Run the migration chain test before publishing.",
  provenance: {
    author: "model",
    origin: {
      chat_id: "chat-1",
      turn_id: "turn-1",
      code_session_id: null,
      code_turn_id: null,
      workspace_id: null,
    },
    evidence: [{ kind: "message", message_id: "message-1" }],
  },
  links: [],
  expires_at: null,
  superseded_by: null,
  observation_count: 0,
  revision: 3,
  created_at: "2026-09-01T09:00:00Z",
  updated_at: "2026-09-01T09:00:00Z",
};

function renderCard(
  client: Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">,
) {
  return render(
    <MemoryProposalCard turnId="turn-1" records={[record]} client={client} />,
  );
}

async function expand(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: /1 memory proposal/ }));
}

describe("MemoryProposalCard", () => {
  it("approves with the held revision and re-renders the returned state", async () => {
    const user = userEvent.setup();
    const client = {
      setMemoryRecordStatus: vi.fn().mockResolvedValue({
        ...record,
        status: "active",
        revision: 4,
      }),
      updateMemoryRecord: vi.fn(),
    } satisfies Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">;
    renderCard(client);

    await expand(user);
    await user.click(screen.getByRole("button", { name: "Approve" }));

    await waitFor(() =>
      expect(client.setMemoryRecordStatus).toHaveBeenCalledWith("record-1", {
        expected_revision: 3,
        status: "active",
      }),
    );
    expect(await screen.findByText("active")).toBeInTheDocument();
    expect(screen.getByText("Memory proposals reviewed")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Approve" }),
    ).not.toBeInTheDocument();
  });

  it("dismisses with the held revision and shows the rejected badge", async () => {
    const user = userEvent.setup();
    const client = {
      setMemoryRecordStatus: vi.fn().mockResolvedValue({
        ...record,
        status: "rejected",
        revision: 4,
      }),
      updateMemoryRecord: vi.fn(),
    } satisfies Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">;
    renderCard(client);

    await expand(user);
    await user.click(screen.getByRole("button", { name: "Dismiss" }));

    await waitFor(() =>
      expect(client.setMemoryRecordStatus).toHaveBeenCalledWith("record-1", {
        expected_revision: 3,
        status: "rejected",
      }),
    );
    expect(await screen.findByText("rejected")).toBeInTheDocument();
  });

  it("saves an edit through the full envelope and shows the returned record", async () => {
    const user = userEvent.setup();
    const client = {
      setMemoryRecordStatus: vi.fn(),
      updateMemoryRecord: vi.fn().mockResolvedValue({
        ...record,
        title: "When changing migrations",
        body: "Run the chain test first.",
        revision: 4,
      }),
    } satisfies Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">;
    renderCard(client);

    await expand(user);
    await user.click(screen.getByRole("button", { name: "Edit" }));
    const title = screen.getByLabelText("Memory title");
    await user.clear(title);
    await user.type(title, "When changing migrations");
    const body = screen.getByLabelText("Memory body");
    await user.clear(body);
    await user.type(body, "Run the chain test first.");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(client.updateMemoryRecord).toHaveBeenCalledWith("record-1", {
        expected_revision: 3,
        kind: "lesson",
        title: "When changing migrations",
        body: "Run the chain test first.",
        author: "model",
        origin: record.provenance.origin,
        evidence: record.provenance.evidence,
        links: [],
        expires_at: null,
        observation_count: 0,
      }),
    );
    expect(
      await screen.findByText("When changing migrations"),
    ).toBeInTheDocument();
    // Still proposed: an edit refines the proposal without deciding it.
    expect(screen.getByRole("button", { name: "Approve" })).toBeInTheDocument();
  });

  it("shows an inline error and keeps the row when a decision fails", async () => {
    const user = userEvent.setup();
    const client = {
      setMemoryRecordStatus: vi
        .fn()
        .mockRejectedValue(new Error("expected_revision does not match")),
      updateMemoryRecord: vi.fn(),
    } satisfies Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">;
    renderCard(client);

    await expand(user);
    await user.click(screen.getByRole("button", { name: "Approve" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      /expected_revision does not match/,
    );
    // The proposal stays actionable rather than vanishing with the failure.
    expect(
      screen.getByText("When changing database migrations"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve" })).toBeInTheDocument();
  });
});
