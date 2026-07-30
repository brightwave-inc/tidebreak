// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, ExecFileChangeSummary } from "./api";
import { ChangeSummaryCard } from "./ChangeSummaryCard";

afterEach(cleanup);

const files: ExecFileChangeSummary[] = [
  {
    snapshot_id: "snapshot-1",
    folder_name: "Project",
    relative_path: "notes.md",
    classification: "applied",
    change: "overwritten",
    rejection_reason: null,
    undo: "available",
    diff: "--- before\n+++ after\n@@ -1 +1 @@\n-old\n+new\n",
  },
  {
    snapshot_id: "rejected:1",
    folder_name: "Project",
    relative_path: "stale.md",
    classification: "rejected",
    change: null,
    rejection_reason: "stale",
    undo: "not_available",
    diff: null,
  },
  {
    snapshot_id: "rejected:2",
    folder_name: "Project",
    relative_path: "deleted.md",
    classification: "rejected",
    change: null,
    rejection_reason: "trash_unavailable",
    undo: "not_available",
    diff: null,
  },
];

describe("ChangeSummaryCard", () => {
  it("separates rejected writes and updates one file after undo", async () => {
    const user = userEvent.setup();
    const client = {
      undoFileChange: vi.fn().mockResolvedValue({
        snapshot_id: "snapshot-1",
        folder_name: "Project",
        relative_path: "notes.md",
        status: "restored",
      }),
      undoTurnFileChanges: vi.fn(),
    } satisfies Pick<
      ApiClient,
      "undoFileChange" | "undoTurnFileChanges"
    >;

    render(
      <ChangeSummaryCard
        client={client}
        chatId="chat-1"
        turnId="turn-1"
        files={files}
      />,
    );

    expect(screen.getByText("3 files touched")).toBeInTheDocument();
    expect(screen.getByText("2 rejected and left unchanged")).toBeInTheDocument();
    expect(
      screen.getByText(/Rejected: file changed before write-back/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Rejected: file could not be moved to trash/),
    ).toBeInTheDocument();

    await user.click(screen.getByText("Text diff"));
    expect(screen.getByText("+new")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() =>
      expect(client.undoFileChange).toHaveBeenCalledWith(
        "chat-1",
        "turn-1",
        "snapshot-1",
      ),
    );
    expect(screen.getByRole("button", { name: "Undone" })).toBeDisabled();
  });
});
