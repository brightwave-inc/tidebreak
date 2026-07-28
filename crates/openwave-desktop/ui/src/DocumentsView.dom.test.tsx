// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DocumentsView,
  type DocumentsApis,
} from "./DocumentsView";
import type { LibraryDocument } from "./documents";

afterEach(cleanup);

const ids = {
  alpha: "6c3df6af-bc62-4a66-a34e-29f327eaef41",
  zeta: "dcf9cc51-465d-438a-bac4-8005c6c980d9",
  queued: "295b4e3d-092e-48ab-9121-1a50c721d676",
  retryable: "096c0f5a-e5f7-46c7-8e6a-d534793a585b",
  permanent: "f079210a-f870-4030-b0b2-c628af729bb6",
};

describe("DocumentsView catalog", () => {
  it("sorts and filters the source table and opens a row from the keyboard", async () => {
    const onOpen = vi.fn();
    const apis = sourceApis([
      source({
        documentId: ids.alpha,
        title: "Alpha notes.md",
        mediaType: "text/markdown",
        sizeBytes: 1_024,
        updatedAt: "2026-07-20T00:00:00Z",
      }),
      source({
        documentId: ids.zeta,
        title: "Zeta report.pdf",
        mediaType: "application/pdf",
        sizeBytes: 2_097_152,
        updatedAt: "2026-07-24T00:00:00Z",
      }),
    ]);
    const user = userEvent.setup();

    render(<DocumentsView chatId="chat-1" onOpen={onOpen} apis={apis} />);
    await screen.findByRole("table");
    expect(openTitleOrder()).toEqual(["Open Zeta report.pdf", "Open Alpha notes.md"]);

    await user.click(screen.getByRole("button", { name: "Title" }));
    expect(screen.getByRole("columnheader", { name: /Title/ })).toHaveAttribute(
      "aria-sort",
      "ascending",
    );
    expect(openTitleOrder()).toEqual(["Open Alpha notes.md", "Open Zeta report.pdf"]);

    await user.type(screen.getByRole("searchbox", { name: "Filter sources" }), "pdf");
    expect(screen.queryByRole("button", { name: "Open Alpha notes.md" })).not.toBeInTheDocument();
    const titleButton = screen.getByRole("button", { name: "Open Zeta report.pdf" });
    titleButton.focus();
    await user.keyboard("{Enter}");
    expect(onOpen).toHaveBeenCalledWith(ids.zeta);
    expect(screen.getByText("PDF")).toBeVisible();
    expect(screen.getByText("2 MB")).toBeVisible();
  });

  it("renders authoritative lifecycle states and gates retry by the server projection", async () => {
    const retryable = source({
      documentId: ids.retryable,
      title: "Retry me.pdf",
      processingStatus: "failed",
      searchable: false,
      failure: {
        message: "The local search index was unavailable. Retry preparing this source.",
        retriable: true,
      },
    });
    const permanent = source({
      documentId: ids.permanent,
      title: "Broken.pdf",
      processingStatus: "failed",
      searchable: false,
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
      searchable: false,
    });
    const ready = source({ documentId: ids.alpha, title: "Ready.md" });
    const unsearchable = source({
      documentId: ids.zeta,
      title: "Scan.pdf",
      searchable: false,
    });
    const afterRetry = {
      ...retryable,
      processingStatus: "queued" as const,
      failure: null,
    };
    const apis = sourceApis([retryable, permanent, queued, ready, unsearchable]);
    vi.mocked(apis.list)
      .mockResolvedValueOnce({
        documents: [retryable, permanent, queued, ready, unsearchable],
        truncated: false,
      })
      .mockResolvedValue({
        documents: [afterRetry, permanent, queued, ready, unsearchable],
        truncated: false,
      });
    const user = userEvent.setup();

    render(<DocumentsView chatId="chat-1" apis={apis} />);
    expect(await screen.findByText("The local search index was unavailable. Retry preparing this source.")).toBeVisible();
    expect(screen.getAllByRole("button", { name: "Retry" })).toHaveLength(1);
    expect(screen.getByText(/Delete it and add a supported/)).toBeVisible();
    expect(screen.getAllByText("Preparing")).toHaveLength(1);
    expect(screen.getByText("Not searchable")).toBeVisible();
    expect(screen.getByText("Ready")).toHaveClass("sr-only");

    await user.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() =>
      expect(apis.retry).toHaveBeenCalledWith("chat-1", ids.retryable),
    );
    await waitFor(() =>
      expect(
        screen.queryByText(
          "The local search index was unavailable. Retry preparing this source.",
        ),
      ).not.toBeInTheDocument(),
    );
    expect(screen.getAllByText("Preparing").length).toBeGreaterThanOrEqual(2);
  });

  it("navigates open, confirms delete, and supports cancelling with Escape", async () => {
    const onOpen = vi.fn();
    const document = source({ documentId: ids.alpha, title: "Notes.md" });
    const apis = sourceApis([document]);
    const user = userEvent.setup();

    render(<DocumentsView chatId="chat-1" onOpen={onOpen} apis={apis} />);
    await screen.findByRole("table");

    await user.click(screen.getByRole("button", { name: "Open Notes.md" }));
    expect(onOpen).toHaveBeenCalledWith(ids.alpha);

    await user.click(screen.getByRole("button", { name: "Delete Notes.md" }));
    expect(await screen.findByRole("alertdialog")).toBeVisible();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(apis.delete).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Delete Notes.md" }));
    await user.click(await screen.findByRole("button", { name: "Delete source" }));
    await waitFor(() =>
      expect(apis.delete).toHaveBeenCalledWith("chat-1", ids.alpha),
    );
    expect(screen.queryByRole("button", { name: "Open Notes.md" })).not.toBeInTheDocument();
  });

  it("ignores a stale catalog response after the conversation changes", async () => {
    let resolveStale:
      | ((catalog: { documents: LibraryDocument[]; truncated: boolean }) => void)
      | undefined;
    const stale = new Promise<{ documents: LibraryDocument[]; truncated: boolean }>(
      (resolve) => {
        resolveStale = resolve;
      },
    );
    const apis = sourceApis([]);
    vi.mocked(apis.list).mockImplementation((chatId) =>
      chatId === "chat-1"
        ? stale
        : Promise.resolve({
            documents: [source({ documentId: ids.zeta, title: "Current.pdf" })],
            truncated: false,
          }),
    );
    const view = render(<DocumentsView chatId="chat-1" apis={apis} />);
    await waitFor(() => expect(apis.list).toHaveBeenCalledWith("chat-1"));

    view.rerender(<DocumentsView chatId="chat-2" apis={apis} />);
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
    const document = source({ documentId: ids.alpha, title: "Scoped.md" });
    const apis = sourceApis([document]);
    const user = userEvent.setup();
    const view = render(<DocumentsView chatId="chat-1" apis={apis} />);
    await screen.findByRole("button", { name: "Delete Scoped.md" });
    await user.click(screen.getByRole("button", { name: "Delete Scoped.md" }));
    expect(await screen.findByRole("alertdialog")).toBeVisible();

    view.rerender(<DocumentsView chatId="chat-2" apis={apis} />);
    await user.click(screen.getByRole("button", { name: "Delete source" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(apis.delete).not.toHaveBeenCalled();
  });

  it("states accepted file behavior and catalog limits when empty", async () => {
    render(<DocumentsView chatId="chat-1" apis={sourceApis([])} />);
    expect(await screen.findByText("No sources yet")).toBeVisible();
    expect(screen.getByText(/files of any type, up to 16 MB each/i)).toBeVisible();
    expect(screen.getByText(/newest 1,000 sources/i)).toBeVisible();
  });
});

function openTitleOrder(): string[] {
  return within(screen.getByRole("table"))
    .getAllByRole("button")
    .map((button) => button.getAttribute("aria-label"))
    .filter((label): label is string => label?.startsWith("Open ") ?? false);
}

function source(overrides: Partial<LibraryDocument> = {}): LibraryDocument {
  return {
    documentId: ids.alpha,
    title: "Source.md",
    mediaType: "text/markdown",
    sizeBytes: 512,
    processingStatus: "ready",
    searchable: true,
    failure: null,
    updatedAt: "2026-07-22T00:00:00Z",
    ...overrides,
  };
}

function sourceApis(documents: LibraryDocument[]): DocumentsApis {
  return {
    list: vi.fn().mockResolvedValue({ documents, truncated: false }),
    import: vi.fn().mockResolvedValue(null),
    search: vi.fn().mockResolvedValue([]),
    delete: vi.fn().mockResolvedValue(undefined),
    retry: vi.fn().mockResolvedValue(undefined),
  };
}
