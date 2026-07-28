// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { ImportQueue } from "./ImportQueue";
import { useImportQueueStore, type ImportQueueEntry } from "./ImportQueueStore";

afterEach(() => {
  cleanup();
  useImportQueueStore.setState({ entries: [] });
});

function entry(overrides: Partial<ImportQueueEntry> = {}): ImportQueueEntry {
  return {
    importId: crypto.randomUUID(),
    displayName: "Report.pdf",
    status: "imported",
    documentId: null,
    processingStatus: null,
    message: null,
    updatedAt: 1,
    ...overrides,
  };
}

function seed(entries: ImportQueueEntry[]) {
  useImportQueueStore.setState({ entries });
}

describe("ImportQueue", () => {
  it("counts what is still in flight and offers no way to dismiss it", async () => {
    seed([
      entry({ displayName: "One.pdf", status: "imported" }),
      entry({ displayName: "Two.docx", status: "streaming" }),
      entry({ displayName: "Three.xlsx", status: "queued" }),
      entry({ displayName: "Four.md", status: "queued" }),
    ]);
    render(<ImportQueue />);

    expect(screen.getByText("Adding 4 sources")).toBeVisible();
    // One of four settled. Per-file, so it steps rather than sweeps.
    expect(screen.getByText("25%")).toBeVisible();
    // Dismissing mid-run would leave the reader with no sight of it.
    expect(screen.queryByRole("button", { name: "Dismiss" })).toBeNull();
  });

  it("counts only what landed once the run is over, and says what failed", async () => {
    seed([
      entry({ displayName: "Good.pdf", status: "imported" }),
      entry({ displayName: "Same.pdf", status: "already_present" }),
      entry({
        displayName: "Broken.pdf",
        status: "failed",
        message: "OpenWave could not read this file.",
      }),
    ]);
    const user = userEvent.setup();
    render(<ImportQueue />);

    // Two of three landed, and a failure is not one of them.
    expect(screen.getByText("Added 2 sources")).toBeVisible();
    expect(screen.getByText("1 source failed")).toBeVisible();
    // The reason sits under its own row rather than in a shared banner.
    expect(screen.getByText("OpenWave could not read this file.")).toBeVisible();
    // A tick cannot say a file was already here, so that one keeps its words.
    expect(screen.getByText("Already added")).toBeVisible();
    expect(screen.queryByText("25%")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(useImportQueueStore.getState().entries).toEqual([]);
  });
});
