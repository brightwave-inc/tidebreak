// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DocumentsView } from "./DocumentsView";
import * as documents from "./documents";
import type { LibraryDocument } from "./documents";

vi.mock("./documents", () => ({
  deleteLibraryDocument: vi.fn(),
  importLibraryDocuments: vi.fn(),
  listLibraryDocuments: vi.fn(),
  retryLibraryDocument: vi.fn(),
  searchLibraryDocuments: vi.fn(),
}));

function source(overrides: Partial<LibraryDocument> = {}): LibraryDocument {
  return {
    documentId: "6c3df6af-bc62-4a66-a34e-29f327eaef41",
    title: "notes.md",
    mediaType: "text/markdown",
    sizeBytes: 2_048,
    processingStatus: "ready",
    failure: null,
    searchable: true,
    createdAt: "2026-07-17T12:00:00Z",
    updatedAt: "2026-07-18T12:00:00Z",
    ...overrides,
  };
}

function listing(...rows: LibraryDocument[]) {
  vi.mocked(documents.listLibraryDocuments).mockResolvedValue({
    documents: rows,
    truncated: false,
  });
}

beforeEach(() => {
  listing();
  // The catalog is the record of what exists, so a delete that reached the
  // server has to be reflected by the next listing as well as by the row going.
  vi.mocked(documents.deleteLibraryDocument).mockImplementation(async () => {
    listing();
  });
  vi.mocked(documents.retryLibraryDocument).mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("DocumentsView", () => {
  it("offers retry only when the failure is one the pipeline could get past", async () => {
    listing(
      source({
        documentId: "11111111-1111-4111-8111-111111111111",
        title: "transient.md",
        processingStatus: "failed",
        searchable: false,
        failure: { reason: "vector_store_failed", retriable: true },
      }),
      source({
        documentId: "22222222-2222-4222-8222-222222222222",
        title: "scan.pdf",
        processingStatus: "failed",
        searchable: false,
        failure: { reason: "parse_failed", retriable: false },
      }),
    );
    render(<DocumentsView chatId="chat-1" />);

    const transient = await screen.findByRole("row", { name: /transient\.md/ });
    const doomed = screen.getByRole("row", { name: /scan\.pdf/ });
    expect(
      within(doomed).getByText("OpenWave could not read what is inside this file."),
    ).toBeInTheDocument();
    expect(within(doomed).queryByRole("button", { name: "Retry" })).toBeNull();

    await userEvent.click(within(transient).getByRole("button", { name: "Retry" }));
    await waitFor(() =>
      expect(documents.retryLibraryDocument).toHaveBeenCalledWith(
        "chat-1",
        "11111111-1111-4111-8111-111111111111",
      ),
    );
  });

  it("removes a source once the reader confirms, and opens one on request", async () => {
    const onOpenDocument = vi.fn();
    listing(source());
    render(<DocumentsView chatId="chat-1" onOpenDocument={onOpenDocument} />);

    await userEvent.click(await screen.findByRole("button", { name: "notes.md" }));
    expect(onOpenDocument).toHaveBeenCalledWith("6c3df6af-bc62-4a66-a34e-29f327eaef41");

    await userEvent.click(screen.getByRole("button", { name: "Actions for notes.md" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Remove" }));
    // Removal is destructive and asynchronous; it must not happen on the way to
    // the confirmation.
    expect(documents.deleteLibraryDocument).not.toHaveBeenCalled();

    await userEvent.click(await screen.findByRole("button", { name: "Remove" }));
    await waitFor(() =>
      expect(documents.deleteLibraryDocument).toHaveBeenCalledWith(
        "chat-1",
        "6c3df6af-bc62-4a66-a34e-29f327eaef41",
      ),
    );
    await waitFor(() => expect(screen.queryByText("notes.md")).toBeNull());
  });

  it("says what can be dropped when the conversation has no sources", async () => {
    render(<DocumentsView chatId="chat-1" />);
    expect(await screen.findByText("No sources yet")).toBeInTheDocument();
    expect(
      screen.getByText(/Folders and empty files are not accepted/),
    ).toBeInTheDocument();
  });
});
