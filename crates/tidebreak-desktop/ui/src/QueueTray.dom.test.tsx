// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, QueuedTurn } from "./api";
import { QueueTray } from "./QueueTray";

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

describe("QueueTray", () => {
  it("moves a queued row first, stops the active turn, and releases the queue", async () => {
    const calls: string[] = [];
    const client = {
      listQueuedTurns: vi.fn().mockResolvedValue({ queued: [first], paused: false }),
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
      <QueueTray client={client} chatId="chat-1" active onStop={onStop} />,
    );
    await screen.findByText("Use the shorter introduction");
    await userEvent.click(
      screen.getByRole("button", { name: "Send queued message 1 now" }),
    );

    await waitFor(() => expect(calls).toEqual(["pause", "move-first", "stop", "release"]));
    expect(client.patchQueuedTurn).toHaveBeenCalledWith("chat-1", "turn-queued", {
      position: 0,
    });
  });
});
