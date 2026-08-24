// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, QueuedCodeTurn, QueuedTurn } from "./api";
import { chatQueueApi, codeQueueApi, QueueTray } from "./QueueTray";

afterEach(cleanup);

const first: QueuedTurn = {
  id: "turn-queued",
  chat_id: "chat-1",
  content: "Use the shorter introduction",
  attachments: [],
  file_attachments: [],
  invoked_skills: [],
  voice_input_used: false,
  position: 1,
  created_at: "2026-08-13T12:00:00Z",
  updated_at: "2026-08-13T12:00:00Z",
};

const codeRow: QueuedCodeTurn = {
  id: "q-1",
  session_id: "sess-1",
  message: "Fix the failing checks on pull request #12",
  position: 0,
  created_at: "2026-08-24T12:00:00Z",
  updated_at: "2026-08-24T12:00:00Z",
};

describe("QueueTray", () => {
  it("moves a queued row first, stops the active turn, and releases the queue", async () => {
    const calls: string[] = [];
    const client = {
      listQueuedTurns: vi
        .fn()
        .mockResolvedValue({ queued: [first], paused: false }),
      putQueuePaused: vi.fn(async (_chatId: string, paused: boolean) => {
        calls.push(paused ? "pause" : "resume");
      }),
      patchQueuedTurn: vi.fn(async () => {
        calls.push("move-first");
        return first;
      }),
      sendQueuedNow: vi.fn(async () => {
        calls.push("release");
      }),
    } as unknown as ApiClient;
    const onStop = vi.fn(async () => {
      calls.push("stop");
    });

    render(
      <QueueTray
        queue={chatQueueApi(client, "chat-1")}
        active
        onStop={onStop}
      />,
    );
    await screen.findByText("Use the shorter introduction");
    await userEvent.click(
      screen.getByRole("button", { name: "Send queued message 1 now" }),
    );

    await waitFor(() =>
      expect(calls).toEqual(["pause", "move-first", "stop", "release"]),
    );
    expect(client.patchQueuedTurn).toHaveBeenCalledWith(
      "chat-1",
      "turn-queued",
      {
        position: 0,
      },
    );
  });

  it("drives a code session's queue through the same tray", async () => {
    const client = {
      listCodeQueuedTurns: vi
        .fn()
        .mockResolvedValue({ queued: [codeRow], paused: false }),
      patchCodeQueuedTurn: vi.fn(async () => codeRow),
      deleteCodeQueuedTurn: vi.fn(async () => undefined),
    } as unknown as ApiClient;

    render(
      <QueueTray
        queue={codeQueueApi(client, "sess-1")}
        active
        onStop={vi.fn(async () => undefined)}
      />,
    );
    const row = await screen.findByText(
      "Fix the failing checks on pull request #12",
    );
    expect(row).toBeInTheDocument();

    // Editing maps the tray's `content` onto the code queue's `message` key.
    await userEvent.click(
      screen.getByRole("button", { name: "Edit queued message" }),
    );
    const box = screen.getByRole("textbox");
    await userEvent.clear(box);
    await userEvent.type(box, "Rebase onto main instead{Enter}");
    await waitFor(() =>
      expect(client.patchCodeQueuedTurn).toHaveBeenCalledWith("sess-1", "q-1", {
        message: "Rebase onto main instead",
      }),
    );

    await userEvent.click(
      screen.getByRole("button", { name: "Delete queued message" }),
    );
    await waitFor(() =>
      expect(client.deleteCodeQueuedTurn).toHaveBeenCalledWith("sess-1", "q-1"),
    );
  });
});
