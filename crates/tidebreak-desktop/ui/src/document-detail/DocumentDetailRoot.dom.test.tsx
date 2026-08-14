// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import {
  HttpError,
  type ApiClient,
  type Chat,
  type DocumentDetail,
} from "@/api";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { AssistantSource } from "@/AssistantSources";
import { useChatListStore } from "@/ChatListStore";
import { useChatSessionStore } from "@/ChatSessionStore";
import type { SheetHighlightRange } from "@/document/UniverSpreadsheetViewer";
import { clearFileDownloadCache } from "@/document/useFileDownload";
import { renderWithRouter } from "../test/router";
import { DocumentDetailRoot } from "./DocumentDetailRoot";

// pdf.js draws to a canvas and runs a worker, neither of which jsdom has, so
// page targeting is observed through a stand-in that keeps the real page state
// hook, which is the part the panel drives.
vi.mock("@/document/PdfViewer", async () => {
  const { usePdfPageState } = await import("@/document/usePdfPageState");
  return {
    PdfViewer: ({
      source,
      targetPage,
    }: {
      source: { id: string };
      targetPage?: number;
    }) => {
      const { currentPage, setCurrentPage } = usePdfPageState(source.id, {
        numPages: 20,
        targetPage,
      });
      return (
        <div>
          <span>Page {currentPage}</span>
          <button type="button" onClick={() => setCurrentPage(currentPage + 1)}>
            Next page
          </button>
        </div>
      );
    },
  };
});

// Spreadsheet viewers render to a canvas through a worker, which jsdom has
// neither of. These stand-ins report the range the panel handed them, which is
// the part the panel is responsible for; selecting and scrolling to those
// cells is the viewer's own behavior.
vi.mock("@/document/NativeSpreadsheetViewer", () => ({
  default: ({ highlightRange }: { highlightRange?: SheetHighlightRange }) => (
    <div>
      {highlightRange
        ? `Sheet ${highlightRange.sheetName} ${highlightRange.startCell}:${highlightRange.endCell}`
        : "Workbook"}
    </div>
  ),
}));

vi.mock("@/document/UniverSpreadsheetViewer", () => ({
  default: ({ highlightRange }: { highlightRange?: SheetHighlightRange }) => (
    <div>
      {highlightRange
        ? `Sheet ${highlightRange.sheetName} ${highlightRange.startCell}:${highlightRange.endCell}`
        : "Workbook"}
    </div>
  ),
}));

const scrolledTo: Element[] = [];

afterEach(() => {
  cleanup();
  useChatSessionStore.getState().reset();
  window.sessionStorage.clear();
  scrolledTo.length = 0;
  // Downloaded bytes are cached for the life of the process, and every case
  // here opens the same document id with different content.
  clearFileDownloadCache();
});

beforeAll(() => {
  // jsdom implements neither half of the object-URL API the image viewer uses.
  URL.createObjectURL = vi.fn(() => "blob:document");
  URL.revokeObjectURL = vi.fn();
  // jsdom does not scroll, so the scrolled-to element is recorded instead.
  Element.prototype.scrollIntoView = function (this: Element) {
    scrolledTo.push(this);
  };
});

function detail(overrides: Partial<DocumentDetail> = {}): DocumentDetail {
  return {
    document_id: "doc-1",
    media_type: "image/png",
    title: "Floor plan.png",
    readable: false,
    has_original_bytes: true,
    updated_at: "2026-07-24T00:00:00Z",
    content: "",
    ...overrides,
  };
}

async function openPanel(
  info: DocumentDetail,
  download = vi.fn(),
  body = "bytes",
) {
  const client = {
    getChatDocument: vi.fn().mockResolvedValue(info),
    getChatDocumentFile: vi.fn().mockResolvedValue({
      bytes: new TextEncoder().encode(body),
      contentType: info.media_type,
    }),
  };
  const rendered = await renderWithRouter(
    <AppContextProvider value={{ client } as unknown as AppContextValue}>
      <DocumentDetailRoot
        chatId="chat-1"
        documentID="doc-1"
        download={download}
        canDownload
      />
    </AppContextProvider>,
    { initialUrl: "/c/chat-1?left=sources.doc-1&right=chat" },
  );
  return { ...rendered, client: client as unknown as ApiClient & typeof client, download };
}

async function openFailingPanel(rejection: unknown) {
  const client = {
    getChatDocument: vi.fn().mockRejectedValue(rejection),
    getChatDocumentFile: vi.fn(),
  };
  const rendered = await renderWithRouter(
    <AppContextProvider value={{ client } as unknown as AppContextValue}>
      <DocumentDetailRoot chatId="chat-1" documentID="doc-1" />
    </AppContextProvider>,
    { initialUrl: "/c/chat-1?left=sources.doc-1&right=chat" },
  );
  return { ...rendered, client };
}

/**
 * The transcript beside the panel, holding the citation the panel is opened
 * from. This is where the panel resolves it: no request is made for it.
 */
function seedTranscript(...sources: (Partial<AssistantSource> & { id: string })[]) {
  useChatSessionStore.getState().update((session) => ({
    ...session,
    messages: [
      {
        id: "m1",
        role: "assistant",
        text: "Revenue rose in the second quarter.",
        sources: sources.map((source, index) => ({
          ordinal: index + 1,
          documentId: "doc-1",
          locator: { kind: "document" },
          ...source,
        })),
      },
    ],
  }));
}

async function openCitation(
  info: DocumentDetail,
  citationId: string,
  body = "bytes",
) {
  const client = {
    getChatDocument: vi.fn().mockResolvedValue(info),
    getChatDocumentFile: vi.fn().mockResolvedValue({
      bytes: new TextEncoder().encode(body),
      contentType: info.media_type,
    }),
  };
  return renderWithRouter(
    <AppContextProvider value={{ client } as unknown as AppContextValue}>
      <DocumentDetailRoot
        chatId="chat-1"
        documentID="doc-1"
        citationId={citationId}
      />
    </AppContextProvider>,
    { initialUrl: `/c/chat-1?left=sources.doc-1.${citationId}&right=chat` },
  );
}

/**
 * The panel opened at one citation, with a control that moves it to another —
 * which is what a second click in the transcript does to the panel already open
 * beside it.
 */
async function openCitationThenAnother(
  info: DocumentDetail,
  first: string,
  second: string,
) {
  const client = {
    getChatDocument: vi.fn().mockResolvedValue(info),
    getChatDocumentFile: vi.fn().mockResolvedValue({
      bytes: new TextEncoder().encode("bytes"),
      contentType: info.media_type,
    }),
  };
  function Harness() {
    const [citationId, setCitationId] = useState(first);
    return (
      <AppContextProvider value={{ client } as unknown as AppContextValue}>
        <button type="button" onClick={() => setCitationId(second)}>
          Click the second citation
        </button>
        <DocumentDetailRoot
          chatId="chat-1"
          documentID="doc-1"
          citationId={citationId}
        />
      </AppContextProvider>
    );
  }
  return renderWithRouter(<Harness />, {
    initialUrl: `/c/chat-1?left=sources.doc-1.${first}&right=chat`,
  });
}

describe("DocumentDetailRoot", () => {
  it("draws and saves the attached original without a catalog breadcrumb", async () => {
    const user = userEvent.setup();
    const { client, download } = await openPanel(detail());

    expect(await screen.findByAltText("Document image")).toHaveAttribute(
      "src",
      "blob:document",
    );
    // Addressed by document id inside its conversation, never by a host path.
    expect(client.getChatDocumentFile).toHaveBeenCalledWith(
      "chat-1",
      "doc-1",
      expect.anything(),
      expect.any(Function),
    );

    await user.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(download).toHaveBeenCalledWith("chat-1", "doc-1"));

    expect(screen.getByText("Document")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Sources" })).not.toBeInTheDocument();
  });

  it("opens a format it cannot draw on the extracted text alone", async () => {
    const { client } = await openPanel(
      detail({
        media_type: "application/vnd.ms-outlook",
        title: "Mailbox.pst",
        content: "Subject: quarterly numbers",
        readable: true,
      }),
    );

    expect(await screen.findByText("Subject: quarterly numbers")).toBeVisible();
    expect(screen.queryByRole("tab", { name: "Original document" })).toBeNull();
    // Nothing is going to draw those bytes, so nothing should pull them over.
    expect(client.getChatDocumentFile).not.toHaveBeenCalled();
  });

  // A fetched web page is stored as the readable text extraction produced; the
  // markup it came from is not kept. Its media type says `text/markdown`, which
  // a viewer would happily accept, so the tab has to be gated on whether there
  // are bytes rather than on whether something could draw them.
  it("offers no original for a source that retained no bytes", async () => {
    const { client } = await openPanel(
      detail({
        media_type: "text/markdown",
        title: "Ownership Explained",
        has_original_bytes: false,
        content: "Ownership moves.",
        readable: true,
      }),
    );

    expect(await screen.findByText("Ownership moves.")).toBeVisible();
    expect(screen.queryByRole("tab", { name: "Original document" })).toBeNull();
    expect(client.getChatDocumentFile).not.toHaveBeenCalled();
  });

  // The tree viewers are reached by media type, and two of these four used to
  // land on the plain-text viewer instead: `text/xml` because it is a text type,
  // and the suffixed types because only the base types were recognised.
  const JSON_BODY = '{"invoice":{"total":42}}';
  const XML_BODY = "<invoice><total>42</total></invoice>";

  it.each([
    ["application/json", JSON_BODY],
    ["application/ld+json", JSON_BODY],
    ["application/xml", XML_BODY],
    ["text/xml", XML_BODY],
  ])("draws %s as a tree rather than as raw text", async (mediaType, body) => {
    await openPanel(detail({ media_type: mediaType, title: "Data" }), vi.fn(), body);

    expect(await screen.findByRole("button", { name: "Collapse all" })).toBeVisible();
    // Decomposed into nodes, so the file's own text never appears as one run.
    expect(screen.queryByText(body)).toBeNull();
  });

  const INVOICES_JSON = '{"invoices":[{"number":"A-1"},{"number":"B-2"}]}';

  // A workbook is opened at a range on a sheet, which is what a citation into
  // one records. A workbook format the panel has no grid viewer for keeps the
  // range on the citation and lands on the extracted text, where the rows it
  // quoted are highlighted instead.
  const SHEET_TEXT = "## Q4 Results\n\n| North | 1204.5 |\n";

  it("opens a citation into a workbook at the range it quoted", async () => {
    seedTranscript({
      id: "cite-cells",
      locator: { kind: "sheet", sheet: "Q4 Results", cells: "B2:D10" },
    });
    await openCitation(
      detail({
        media_type:
          "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        title: "Q4.xlsx",
        content: SHEET_TEXT,
      }),
      "cite-cells",
    );

    expect(
      await screen.findByText("Sheet Q4 Results B2:D10"),
    ).toBeVisible();
  });

  it("falls back to the extracted text for a workbook it cannot draw", async () => {
    seedTranscript({
      id: "cite-ods",
      locator: { kind: "sheet", sheet: "Q4 Results", cells: "B2:D10" },
    });
    await openCitation(
      detail({
        media_type: "application/vnd.oasis.opendocument.spreadsheet",
        title: "Q4.ods",
        content: SHEET_TEXT,
      }),
      "cite-ods",
    );

    await waitFor(() =>
      expect(document.querySelector("pre")?.textContent).toContain("Q4 Results"),
    );
    expect(screen.queryByText(/^Sheet /)).toBeNull();
  });

  it("opens a tree collapsed when nothing pointed into it", async () => {
    await openPanel(
      detail({ media_type: "application/json", title: "Invoices" }),
      vi.fn(),
      INVOICES_JSON,
    );

    expect(await screen.findByRole("button", { name: "Collapse all" })).toBeVisible();
    expect(screen.queryByText(/B-2/)).toBeNull();
  });

  // The outline slugs the raw markdown and the renderer slugs the rendered
  // heading, independently. This is the only test that puts the two together,
  // so it is what would catch them drifting apart.
  it("lists a markdown source's headings and scrolls to one", async () => {
    const user = userEvent.setup();
    await openPanel(
      detail({ media_type: "text/markdown", title: "Report.md" }),
      vi.fn(),
      "# Quarterly report\n\nBody.\n\n## Revenue by **segment**\n\nMore.\n",
    );

    await user.click(await screen.findByRole("button", { name: "Document outline" }));
    const entry = await screen.findByRole("button", { name: "Revenue by segment" });

    const scrollIntoView = vi.fn();
    const heading = document.querySelector("#revenue-by-segment");
    expect(heading).not.toBeNull();
    heading!.scrollIntoView = scrollIntoView;

    await user.click(entry);
    expect(scrollIntoView).toHaveBeenCalled();
  });

  it("offers no outline for a source that is text rather than markdown", async () => {
    await openPanel(
      detail({ media_type: "text/plain", title: "Notes.txt" }),
      vi.fn(),
      "# Not a heading, just a line that starts with a hash\n",
    );

    expect(
      await screen.findByText(/Not a heading, just a line that starts with a hash/),
    ).toBeVisible();
    expect(screen.queryByRole("button", { name: "Document outline" })).toBeNull();
  });

  const CITED_CONTENT = "Café notes\n\nRevenue rose 12% in the second quarter.\n";
  const CITED_PASSAGE = "Revenue rose 12%";

  it("opens a citation on the line the model named", async () => {
    seedTranscript({
      id: "cite-1",
      locator: { kind: "lines", start: 3, end: 3 },
    });
    await openCitation(
      // A source whose original view could be drawn, but cannot show a span:
      // the citation lands on the extracted text, where the passage is.
      detail({ media_type: "text/plain", title: "Notes.txt", content: CITED_CONTENT }),
      "cite-1",
    );

    await waitFor(() => expect(document.querySelector("mark")).not.toBeNull());
    const cited = document.querySelector("mark")!;
    expect(cited.textContent).toContain(CITED_PASSAGE);
    expect(scrolledTo).toContain(cited);
    // Split around the passage rather than reduced to it: the text of record
    // still reads as one run, which is what the offsets index into.
    expect(document.querySelector("pre")?.textContent).toBe(CITED_CONTENT);
  });

  it("opens the same source at the top when no citation led there", async () => {
    await openPanel(
      detail({ media_type: "text/plain", title: "Notes.txt", content: CITED_CONTENT }),
    );

    expect(
      await screen.findByRole("tab", { name: "Original document", selected: true }),
    ).toBeVisible();
    expect(document.querySelector("mark")).toBeNull();
  });

  const MARKDOWN_CONTENT =
    "# Café notes\n\nQuarter one was flat.\nRevenue rose **12%** in the second quarter.\n";

  it("marks the cited line in a rendered markdown original", async () => {
    const user = userEvent.setup();
    seedTranscript({
      id: "cite-3",
      locator: { kind: "lines", start: 4, end: 4 },
    });
    await openCitation(
      detail({
        media_type: "text/markdown",
        title: "Report.md",
        content: MARKDOWN_CONTENT,
      }),
      "cite-3",
      MARKDOWN_CONTENT,
    );

    await user.click(await screen.findByRole("tab", { name: "Original document" }));
    await screen.findByText(/Quarter one was flat/);

    // The passage spans an emphasized word, which is three places in the
    // rendered tree rather than one run: the words between them are not
    // adjacent there, so each is marked where it stands.
    await waitFor(() => expect(document.querySelectorAll("mark").length).toBeGreaterThan(0));
    const marks = Array.from(document.querySelectorAll("mark"));
    expect(marks.map((mark) => mark.textContent).join("")).toContain(
      "Revenue rose 12% in the second",
    );
    expect(document.querySelector("strong mark")?.textContent).toBe("12%");
    expect(scrolledTo).toContain(marks[0]);
  });

  it("marks the cited line in an original drawn as plain text", async () => {
    const user = userEvent.setup();
    seedTranscript({
      id: "cite-4",
      locator: { kind: "lines", start: 3, end: 3 },
    });
    await openCitation(
      detail({ media_type: "text/plain", title: "Notes.txt", content: CITED_CONTENT }),
      "cite-4",
      CITED_CONTENT,
    );

    await user.click(await screen.findByRole("tab", { name: "Original document" }));

    await waitFor(() => expect(document.querySelector("mark")).not.toBeNull());
    const marks = Array.from(document.querySelectorAll("mark"));
    expect(marks.every((mark) => mark.textContent?.includes(CITED_PASSAGE))).toBe(true);
    // Drawn as written, the file still reads as one run around the mark.
    expect(document.querySelector("pre")?.textContent).toBe(CITED_CONTENT);
  });

  it("opens a paginated citation on its recorded page, then lets the reader leave", async () => {
    const user = userEvent.setup();
    seedTranscript({
      id: "cite-2",
      locator: { kind: "pages", start: 4, end: 5 },
    });
    await openCitation(
      detail({ media_type: "application/pdf", title: "Report.pdf", content: "text" }),
      "cite-2",
    );

    // The span starts on page 4; the page remembered for this source is not it.
    expect(await screen.findByText("Page 4")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Next page" }));
    expect(await screen.findByText("Page 5")).toBeVisible();

    // The transcript keeps arriving while the panel is open, and the citation is
    // re-resolved with it. The page it asked for must not be applied twice.
    seedTranscript({
      id: "cite-2",
      locator: { kind: "pages", start: 4, end: 5 },
    });
    expect(await screen.findByText("Page 5")).toBeVisible();
  });

  // The panel is already open at a citation when the next one is clicked, and
  // two citations into one source usually want the same view — so the view a
  // citation asks for cannot tell the second click from the first. A reader who
  // had switched views in between used to stay where they were, and the second
  // citation landed nowhere.
  it("lands a second citation into a source the reader had switched views on", async () => {
    const user = userEvent.setup();
    seedTranscript(
      { id: "cite-6", locator: { kind: "page", page: 2 } },
      { id: "cite-7", locator: { kind: "page", page: 7 } },
    );
    await openCitationThenAnother(
      detail({ media_type: "application/pdf", title: "Report.pdf", content: "text" }),
      "cite-6",
      "cite-7",
    );

    expect(await screen.findByText("Page 2")).toBeVisible();
    await user.click(screen.getByRole("tab", { name: "Extracted text" }));

    await user.click(screen.getByRole("button", { name: "Click the second citation" }));

    expect(
      await screen.findByRole("tab", { name: "Original document", selected: true }),
    ).toBeVisible();
    expect(await screen.findByText("Page 7")).toBeVisible();
  });

  it("draws the original from cache when the reader flips views and back", async () => {
    const user = userEvent.setup();
    const { client } = await openPanel(
      detail({
        media_type: "text/markdown",
        title: "Report.md",
        content: "The text of record.",
        readable: true,
      }),
      vi.fn(),
      "# Quarterly report\n",
    );

    expect(await screen.findByText("Quarterly report")).toBeVisible();
    await user.click(screen.getByRole("tab", { name: "Extracted text" }));
    expect(await screen.findByText("The text of record.")).toBeVisible();
    await user.click(screen.getByRole("tab", { name: "Original document" }));

    expect(await screen.findByText("Quarterly report")).toBeVisible();
    expect(client.getChatDocumentFile).toHaveBeenCalledTimes(1);
  });

  it("says so when a structured source will not parse", async () => {
    await openPanel(
      detail({ media_type: "application/json", title: "Truncated.json" }),
      vi.fn(),
      '{"invoice": ',
    );

    expect(await screen.findByText("Unable to parse JSON")).toBeVisible();
  });
});

// Every way this load can fail used to read as "the document is no longer
// available", which is a claim about the document that only a 404 supports.
describe("DocumentDetailRoot load failures", () => {
  it("says a source is gone only when the server says it is gone", async () => {
    await openFailingPanel(new HttpError(404, "404: document not found"));

    expect(
      await screen.findByText("The document is no longer available."),
    ).toBeVisible();
    // Nothing to retry: asking again cannot bring it back.
    expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  });

  it("reports a server failure as a failure, and asks again on request", async () => {
    const { client } = await openFailingPanel(new HttpError(500, "500: internal error"));

    expect(
      await screen.findByText("The document could not be loaded (500)."),
    ).toBeVisible();
    expect(screen.queryByText("The document is no longer available.")).toBeNull();

    // A retry that succeeds leaves the reader on the document. A format with no
    // viewer opens on its extracted text, so no byte fetch is needed to see it.
    client.getChatDocument.mockResolvedValue(
      detail({
        media_type: "application/vnd.ms-outlook",
        title: "Mailbox.pst",
        content: "Recovered.",
        readable: true,
      }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("Recovered.")).toBeVisible();
  });

  it("reports a dropped connection as a failure rather than a deletion", async () => {
    await openFailingPanel(new TypeError("Load failed"));

    expect(await screen.findByText("The document could not be loaded.")).toBeVisible();
    expect(screen.getByRole("button", { name: "Try again" })).toBeVisible();
  });
});

describe("sharing an attachment with the project", () => {
  afterEach(() => {
    useChatListStore.getState().setChats([]);
  });

  async function openPanelForChat(chat: Partial<Chat> & { id: string }) {
    useChatListStore.getState().setChats([chat as Chat]);
    const client = {
      getChatDocument: vi.fn().mockResolvedValue(detail()),
      getChatDocumentFile: vi.fn().mockResolvedValue({
        bytes: new TextEncoder().encode("bytes"),
        contentType: "image/png",
      }),
      promoteDocumentToProject: vi.fn().mockResolvedValue({ document_id: "doc-2" }),
    };
    await renderWithRouter(
      <AppContextProvider value={{ client } as unknown as AppContextValue}>
        <DocumentDetailRoot chatId={chat.id} documentID="doc-1" />
      </AppContextProvider>,
      { initialUrl: `/c/${chat.id}?left=sources.doc-1&right=chat` },
    );
    return client;
  }

  it("offers the project only to a conversation that is in one", async () => {
    const client = await openPanelForChat({
      id: "chat-1",
      project_id: "project-1",
    });
    await userEvent.click(
      await screen.findByRole("button", { name: "Add to project" }),
    );
    await waitFor(() =>
      expect(client.promoteDocumentToProject).toHaveBeenCalledWith(
        "project-1",
        "chat-1",
        "doc-1",
      ),
    );
    // The click is spent: a second one would only make the same project file
    // again, so the control reports the state it reached instead.
    expect(await screen.findByRole("button", { name: "In the project" })).toBeDisabled();
  });

  it("offers nothing to a loose conversation, which has no project to share with", async () => {
    await openPanelForChat({ id: "chat-1", project_id: null });
    // Wait for the document itself, so the absence is a decision rather than
    // the panel not having loaded yet.
    expect(await screen.findByText("Floor plan.png")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add to project" })).toBeNull();
  });
});
