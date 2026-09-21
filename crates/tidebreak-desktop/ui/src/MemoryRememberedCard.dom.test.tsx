// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, MemoryRecord } from "./api";
import { MemoryRememberedCard } from "./MemoryRememberedCard";

afterEach(() => {
  cleanup();
});

const record: MemoryRecord = {
  id: "record-1",
  scope: { kind: "personal" },
  kind: "lesson",
  status: "active",
  title: "When changing database migrations",
  body: "Run the migration chain test before publishing.",
  provenance: {
    author: "model",
    origin: {
      chat_id: "chat-1",
      turn_id: "turn-1",
      code_session_id: null,
    },
    evidence: [{ kind: "message", message_id: "message-1" }],
  },
  links: [],
  expires_at: null,
  superseded_by: null,
  observation_count: 1,
  revision: 3,
  created_at: "2026-09-01T09:00:00Z",
  updated_at: "2026-09-01T09:00:00Z",
};

type Client = Pick<ApiClient, "setMemoryRecordStatus" | "updateMemoryRecord">;

function renderCard(client: Client) {
  return render(
    <MemoryRememberedCard turnId="turn-1" records={[record]} client={client} />,
  );
}

async function expand(user: ReturnType<typeof userEvent.setup>) {
  await user.click(
    screen.getByRole("button", {
      name: /Remembered: When changing database migrations/,
    }),
  );
}

describe("MemoryRememberedCard", () => {
  it("names what was remembered and forgets with the held revision", async () => {
    const user = userEvent.setup();
    const client = {
      setMemoryRecordStatus: vi.fn().mockResolvedValue({
        ...record,
        status: "archived",
        revision: 4,
      }),
      updateMemoryRecord: vi.fn(),
    } satisfies Client;
    renderCard(client);
    await expand(user);
    await user.click(screen.getByRole("button", { name: "Forget" }));
    expect(client.setMemoryRecordStatus).toHaveBeenCalledWith("record-1", {
      expected_revision: 3,
      status: "archived",
    });
    expect(await screen.findByText("Forgotten")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Forget" }),
    ).not.toBeInTheDocument();
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
    } satisfies Client;
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
    expect(client.updateMemoryRecord).toHaveBeenCalledWith(
      "record-1",
      expect.objectContaining({
        expected_revision: 3,
        title: "When changing migrations",
        body: "Run the chain test first.",
        author: "model",
      }),
    );
    expect(
      await screen.findByText("Run the chain test first."),
    ).toBeInTheDocument();
  });

  it("shows an inline error and keeps the row when forgetting fails", async () => {
    const user = userEvent.setup();
    const client = {
      setMemoryRecordStatus: vi
        .fn()
        .mockRejectedValue(new Error("revision conflict")),
      updateMemoryRecord: vi.fn(),
    } satisfies Client;
    renderCard(client);
    await expand(user);
    await user.click(screen.getByRole("button", { name: "Forget" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "revision conflict",
    );
    expect(screen.getByRole("button", { name: "Forget" })).toBeInTheDocument();
  });
});
