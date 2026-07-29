// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { LibraryDocument } from "@/documents";
import { SourcesView, type SourcesApis } from "./SourcesView";

afterEach(cleanup);

const ids = {
  alpha: "6c3df6af-bc62-4a66-a34e-29f327eaef41",
  zeta: "dcf9cc51-465d-438a-bac4-8005c6c980d9",
  queued: "295b4e3d-092e-48ab-9121-1a50c721d676",
  retryable: "096c0f5a-e5f7-46c7-8e6a-d534793a585b",
  permanent: "f079210a-f870-4030-b0b2-c628af729bb6",
};

describe("SourcesView catalog", () => {
  it("searches the catalog and opens a row", async () => {
    const onOpen = vi.fn();
    const apis = sourceApis([
      source({
        documentId: ids.alpha,
        title: "Alpha notes.md",
        mediaType: "text/markdown",
        sizeBytes: 1_024,
      }),
      source({
        documentId: ids.zeta,
        title: "Zeta report.pdf",
        mediaType: "application/pdf",
        sizeBytes: 2_097_152,
      }),
    ]);
    const user = userEvent.setup();

    render(<SourcesView chatId="chat-1" onOpen={onOpen} apis={apis} />);
    await screen.findByRole("button", { name: "Open Alpha notes.md" });
    expect(screen.getByText("PDF")).toBeVisible();
    expect(screen.getByText("2 MB")).toBeVisible();

    await user.type(screen.getByPlaceholderText("Search sources…"), "pdf");
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Open Alpha notes.md" }),
      ).not.toBeInTheDocument(),
    );

    await user.click(screen.getByRole("button", { name: "Open Zeta report.pdf" }));
    expect(onOpen).toHaveBeenCalledWith(ids.zeta);
  });

  it("filters the catalog down to one type and reports the narrowed count", async () => {
    const apis = sourceApis([
      source({ documentId: ids.alpha, title: "Notes.md", mediaType: "text/markdown" }),
      source({ documentId: ids.zeta, title: "Report.pdf", mediaType: "application/pdf" }),
    ]);
    const user = userEvent.setup();

    render(<SourcesView chatId="chat-1" apis={apis} />);
    await screen.findByRole("button", { name: "Open Notes.md" });
    expect(screen.getByText("2")).toBeVisible();

    await user.click(screen.getByRole("button", { name: /^Type/ }));
    await user.click(await screen.findByRole("checkbox", { name: /PDF/ }));

    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Open Notes.md" })).not.toBeInTheDocument(),
    );
    expect(screen.getByText("showing 1 of 2")).toBeVisible();
  });

  it("shows lifecycle states and offers retry only where the server allows it", async () => {
    const retryable = source({
      documentId: ids.retryable,
      title: "Retry me.pdf",
      processingStatus: "failed",
      readable: false,
      failure: {
        message: "The local search index was unavailable. Retry preparing this source.",
        retriable: true,
      },
    });
    const permanent = source({
      documentId: ids.permanent,
      title: "Broken.pdf",
      processingStatus: "failed",
      readable: false,
      failure: {
        message:
          "OpenWave could not read this file. Delete it and add a supported, uncorrupted version.",
        retriable: false,
      },
    });
    const queued = source({
      documentId: ids.queued,
      title: "Preparing.md",
      processingStatus: "processing",
      readable: false,
    });
    const unreadable = source({ documentId: ids.zeta, title: "Scan.pdf", readable: false });
    const ready = source({ documentId: ids.alpha, title: "Ready.md" });
    const apis = sourceApis([retryable, permanent, queued, unreadable, ready]);
    vi.mocked(apis.list)
      .mockResolvedValueOnce({
        documents: [retryable, permanent, queued, unreadable, ready],
        truncated: false,
      })
      .mockResolvedValue({
        documents: [
          { ...retryable, processingStatus: "queued", failure: null },
          permanent,
          queued,
          unreadable,
          ready,
        ],
        truncated: false,
      });
    const user = userEvent.setup();

    render(<SourcesView chatId="chat-1" apis={apis} />);
    await screen.findByRole("button", { name: "Open Ready.md" });

    // A ready, readable source is the quiet case and wears no pill at all.
    expect(screen.getAllByText("Failed")).toHaveLength(2);
    expect(screen.getByText("Preparing")).toBeVisible();
    expect(screen.getByText("No text")).toBeVisible();
    // Only the retriable failure offers the action.
    expect(screen.getAllByRole("button", { name: "Retry" })).toHaveLength(1);

    await user.click(screen.getAllByText("Failed")[0]!);
    expect(
      await screen.findByText(/The local search index was unavailable|could not read this file/),
    ).toBeVisible();
    await user.keyboard("{Escape}");

    await user.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(apis.retry).toHaveBeenCalledWith("chat-1", ids.retryable));
    await waitFor(() => expect(screen.getAllByText("Failed")).toHaveLength(1));
  });

  it("confirms a delete and lets Escape cancel it", async () => {
    const apis = sourceApis([source({ documentId: ids.alpha, title: "Notes.md" })]);
    const user = userEvent.setup();

    render(<SourcesView chatId="chat-1" apis={apis} />);
    await screen.findByRole("button", { name: "Open Notes.md" });

    await user.click(screen.getByRole("button", { name: "More options for Notes.md" }));
    await user.click(await screen.findByRole("menuitem", { name: "Delete" }));
    expect(await screen.findByRole("alertdialog")).toBeVisible();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(apis.delete).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "More options for Notes.md" }));
    await user.click(await screen.findByRole("menuitem", { name: "Delete" }));
    await user.click(await screen.findByRole("button", { name: "Delete source" }));
    await waitFor(() => expect(apis.delete).toHaveBeenCalledWith("chat-1", ids.alpha));
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Open Notes.md" })).not.toBeInTheDocument(),
    );
  });

  it("ignores a stale catalog response after the conversation changes", async () => {
    let resolveStale: ((catalog: { documents: LibraryDocument[]; truncated: boolean }) => void) | undefined;
    const stale = new Promise<{ documents: LibraryDocument[]; truncated: boolean }>((resolve) => {
      resolveStale = resolve;
    });
    const apis = sourceApis([]);
    vi.mocked(apis.list).mockImplementation((chatId) =>
      chatId === "chat-1"
        ? stale
        : Promise.resolve({
            documents: [source({ documentId: ids.zeta, title: "Current.pdf" })],
            truncated: false,
          }),
    );

    const view = render(<SourcesView chatId="chat-1" apis={apis} />);
    await waitFor(() => expect(apis.list).toHaveBeenCalledWith("chat-1"));

    view.rerender(<SourcesView chatId="chat-2" apis={apis} />);
    expect(await screen.findByRole("button", { name: "Open Current.pdf" })).toBeVisible();
    await act(async () => {
      resolveStale?.({
        documents: [source({ documentId: ids.alpha, title: "Stale.md" })],
        truncated: false,
      });
    });
    expect(screen.getByRole("button", { name: "Open Current.pdf" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "Open Stale.md" })).not.toBeInTheDocument();
  });

  it("does not apply a pending delete confirmation to a different conversation", async () => {
    const apis = sourceApis([source({ documentId: ids.alpha, title: "Scoped.md" })]);
    const user = userEvent.setup();

    const view = render(<SourcesView chatId="chat-1" apis={apis} />);
    await screen.findByRole("button", { name: "Open Scoped.md" });
    await user.click(screen.getByRole("button", { name: "More options for Scoped.md" }));
    await user.click(await screen.findByRole("menuitem", { name: "Delete" }));
    expect(await screen.findByRole("alertdialog")).toBeVisible();

    view.rerender(<SourcesView chatId="chat-2" apis={apis} />);
    await user.click(screen.getByRole("button", { name: "Delete source" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(apis.delete).not.toHaveBeenCalled();
  });

  it("states what a conversation accepts when it has no sources yet", async () => {
    render(<SourcesView chatId="chat-1" apis={sourceApis([])} />);
    expect(await screen.findByText("No sources yet")).toBeVisible();
    // The two things a reader needs before dropping anything: that an
    // unreadable format is still kept, and the size ceiling.
    expect(screen.getByText(/still kept as a source/i)).toBeVisible();
    expect(screen.getByText(/Maximum file size: 16MB/i)).toBeVisible();
  });
});

function source(overrides: Partial<LibraryDocument> = {}): LibraryDocument {
  return {
    documentId: ids.alpha,
    title: "Source.md",
    mediaType: "text/markdown",
    sizeBytes: 512,
    processingStatus: "ready",
    readable: true,
    failure: null,
    updatedAt: "2026-07-22T00:00:00Z",
    ...overrides,
  };
}

function sourceApis(documents: LibraryDocument[]): SourcesApis {
  return {
    list: vi.fn().mockResolvedValue({ documents, truncated: false }),
    import: vi.fn().mockResolvedValue(null),
    delete: vi.fn().mockResolvedValue(undefined),
    retry: vi.fn().mockResolvedValue(undefined),
    export: vi.fn().mockResolvedValue(true),
  };
}
