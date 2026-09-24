// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, QueuedCodeTurn, QueuedTurn } from "./api";
import { usePendingReviewStore } from "./code/diff/pendingReview";
import {
  messageWithReviewComments,
  reviewBlockOf,
  type ReviewComment,
} from "./code/diff/reviewComments";
import { chatQueueApi, codeQueueApi, QueueTray } from "./QueueTray";

afterEach(() => {
  cleanup();
  usePendingReviewStore.setState({ byWorkspace: {}, sending: {}, queued: {} });
});

const first: QueuedTurn = {
  id: "turn-queued",
  session_id: "chat-1",
  message: "Use the shorter introduction",
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

  describe("a queued message that carries diff comments", () => {
    const review: ReviewComment = {
      id: "c1",
      author: { kind: "person" },
      path: "src/queue.ts",
      lines: [
        { kind: "del", oldNo: 22, newNo: null, text: "const MAX = 10;" },
        { kind: "add", oldNo: null, newNo: 23, text: "const MAX = 20;" },
      ],
      body: "Why double it?",
      createdAt: "2026-09-24T10:00:00.000Z",
    };

    function queued(message: string) {
      const row = { ...codeRow, message };
      return {
        listCodeQueuedTurns: vi
          .fn()
          .mockResolvedValue({ queued: [row], paused: false }),
        patchCodeQueuedTurn: vi.fn(async () => row),
        deleteCodeQueuedTurn: vi.fn(async () => undefined),
      } as unknown as ApiClient & {
        patchCodeQueuedTurn: ReturnType<typeof vi.fn>;
      };
    }

    it("counts the comments, edits only the text, and keeps the block", async () => {
      const message = messageWithReviewComments("Then run the tests.", [
        review,
      ]);
      const client = queued(message);
      const { container } = render(
        <QueueTray
          queue={codeQueueApi(client, "sess-1", { workspaceId: "ws-1" })}
          active
          onStop={vi.fn(async () => undefined)}
        />,
      );
      expect(await screen.findByText("Then run the tests.")).toBeVisible();
      expect(screen.getByText("1 review comment")).toBeVisible();
      expect(container.innerHTML).not.toContain("review_comments");

      await userEvent.click(
        screen.getByRole("button", { name: "Edit queued message" }),
      );
      const box = screen.getByRole("textbox");
      expect(box).toHaveValue("Then run the tests.");
      await userEvent.clear(box);
      await userEvent.type(box, "Run them twice{Enter}");
      await waitFor(() =>
        expect(client.patchCodeQueuedTurn).toHaveBeenCalledWith(
          "sess-1",
          "q-1",
          { message: `Run them twice\n\n${reviewBlockOf(message)}` },
        ),
      );
    });

    it("gives back every comment whole, as it was when the message went", async () => {
      // A comment on a whitespace pair, with the code around it, written on
      // a turn of another conversation: the block names that turn only as
      // "an earlier turn", and keeps neither the pair's old text nor the
      // code around the lines.
      const whole: ReviewComment = {
        id: "c-pair",
        author: { kind: "person" },
        path: "src/layout.ts",
        turnId: "turn-of-another-conversation",
        lines: [
          {
            kind: "context",
            oldNo: 41,
            newNo: 41,
            text: "  layout();",
            oldText: "\tlayout();",
          },
        ],
        context: { before: ["open();"], after: ["close();"] },
        body: "Tabs here, please.",
        createdAt: "2026-09-24T09:00:00.000Z",
      };
      const message = messageWithReviewComments("Then run the tests.", [
        whole,
        review,
      ]);
      expect(message).toContain('diff="an earlier turn"');
      usePendingReviewStore
        .getState()
        .keepQueued("ws-1", reviewBlockOf(message)!, [whole, review]);
      const client = queued(message);
      render(
        <QueueTray
          queue={codeQueueApi(client, "sess-1", {
            workspaceId: "ws-1",
            turnFor: () => null,
          })}
          active
          onStop={vi.fn(async () => undefined)}
        />,
      );
      await userEvent.click(
        await screen.findByRole("button", { name: "Delete queued message" }),
      );
      await waitFor(() =>
        expect(usePendingReviewStore.getState().byWorkspace["ws-1"]).toEqual([
          whole,
          review,
        ]),
      );
      expect(usePendingReviewStore.getState().queued["ws-1"]).toBeUndefined();
    });

    it("gives the comments back to the review when the message is deleted", async () => {
      const client = queued(messageWithReviewComments("", [review]));
      render(
        <QueueTray
          queue={codeQueueApi(client, "sess-1", { workspaceId: "ws-1" })}
          active
          onStop={vi.fn(async () => undefined)}
        />,
      );
      // A message of comments alone reads as its count.
      expect(await screen.findByText("1 review comment")).toBeVisible();
      await userEvent.click(
        screen.getByRole("button", { name: "Delete queued message" }),
      );
      await waitFor(() =>
        expect(usePendingReviewStore.getState().byWorkspace["ws-1"]).toEqual([
          expect.objectContaining({
            path: "src/queue.ts",
            body: "Why double it?",
            lines: [
              { kind: "del", oldNo: 22, newNo: null, text: "const MAX = 10;" },
              { kind: "add", oldNo: null, newNo: 23, text: "const MAX = 20;" },
            ],
          }),
        ]),
      );
    });
  });
});
