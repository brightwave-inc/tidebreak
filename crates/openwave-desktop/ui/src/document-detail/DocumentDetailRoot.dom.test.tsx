// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import {
  HttpError,
  type ApiClient,
  type CitationPageBounds,
  type DocumentDetail,
} from "@/api";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { AssistantSource } from "@/AssistantSources";
import { useChatSessionStore } from "@/ChatSessionStore";
import { CITATION_MARK_CLASS } from "@/components/document/citationMark";
import type { SheetHighlightRange } from "@/document/UniverSpreadsheetViewer";
import { clearFileDownloadCache } from "@/document/useFileDownload";
import { renderWithRouter } from "../test/router";
import { DocumentDetailRoot } from "./DocumentDetailRoot";

// pdf.js draws to a canvas and runs a worker, neither of which jsdom has, so
// the page targeting is observed through a stand-in that keeps the real page
// state hook and the real highlight overlay — the parts the panel drives.
vi.mock("@/document/PdfViewer", async () => {
  const { usePdfPageState } = await import("@/document/usePdfPageState");
  const { PdfPageHighlights } = await import("@/document/PdfPageHighlights");
  return {
    PdfViewer: ({
      documentId,
      targetPage,
      highlights,
    }: {
      documentId: string;
      targetPage?: number;
      highlights?: readonly CitationPageBounds[];
    }) => {
      const { currentPage, setCurrentPage } = usePdfPageState(documentId, {
        numPages: 20,
        targetPage,
      });
      return (
        <div>
          <span>Page {currentPage}</span>
          <button type="button" onClick={() => setCurrentPage(currentPage + 1)}>
            Next page
          </button>
          <PdfPageHighlights
            page={currentPage}
            highlights={highlights ?? []}
            onNavigate={setCurrentPage}
          />
        </div>
      );
    },
  };
});

// Univer renders to a canvas through a worker, which jsdom has neither of. The
// stand-in reports the range the panel handed it, which is the part the panel
// is responsible for; selecting and scrolling to those cells is the viewer's
// own, and it already did that before anything produced a range.
vi.mock("@/document/UniverSpreadsheetViewer", () => ({
  default: ({ highlightRange }: { highlightRange?: SheetHighlightRange }) => (
    <div>
      {highlightRange
        ? `Sheet ${highlightRange.sheetName} ${highlightRange.startCell}:${highlightRange.endCell}`
        : "Workbook"}
    </div>
  ),
}));

/** The boxes drawn over the page on screen, however many pages carry them. */
function drawnHighlights(): Element[] {
  return [...document.querySelectorAll(`.${CITATION_MARK_CLASS}`)];
}

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
    processing_status: "ready",
    searchable: false,
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
        position="left"
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
      <DocumentDetailRoot chatId="chat-1" documentID="doc-1" position="left" />
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
          span: { start: 0, end: 0 },
          excerpt: "",
          heading: null,
          pages: [],
          bounds: [],
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
        position="left"
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
          position="left"
        />
      </AppContextProvider>
    );
  }
  return renderWithRouter(<Harness />, {
    initialUrl: `/c/chat-1?left=sources.doc-1.${first}&right=chat`,
  });
}

/** One rectangle of a citation, in the ten-thousandths the wire carries. */
function rect(page: number, bounds: CitationPageBounds["bounds"]): CitationPageBounds {
  return { page, bounds };
}

/** Byte offsets of a passage, which is how a citation reports its span. */
function byteSpan(text: string, passage: string) {
  const encoder = new TextEncoder();
  const start = encoder.encode(text.slice(0, text.indexOf(passage))).length;
  return { start, end: start + encoder.encode(passage).length };
}

describe("DocumentDetailRoot", () => {
  it("draws the original, saves it, and leads back to the list", async () => {
    const user = userEvent.setup();
    const { client, download, router } = await openPanel(detail());

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

    await user.click(screen.getByRole("button", { name: "Sources" }));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "sources", right: "chat" }),
    );
  });

  it("opens a format it cannot draw on the extracted text alone", async () => {
    const { client } = await openPanel(
      detail({
        media_type: "application/vnd.ms-outlook",
        title: "Mailbox.pst",
        content: "Subject: quarterly numbers",
        searchable: true,
      }),
    );

    expect(await screen.findByText("Subject: quarterly numbers")).toBeVisible();
    expect(screen.queryByRole("tab", { name: "Original document" })).toBeNull();
    // Nothing is going to draw those bytes, so nothing should pull them over.
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

  // A tree is navigated by node, so a citation into one carries the path of the
  // node it quoted beside its span. Everything below the root opens collapsed,
  // which is why the quoted value is only on screen when the path arrived: it
  // is what expands the record holding it.
  const INVOICES_JSON = '{"invoices":[{"number":"A-1"},{"number":"B-2"}]}';
  const INVOICES_XML =
    "<invoices><invoice><number>A-1</number></invoice><invoice><number>B-2</number></invoice></invoices>";

  it.each([
    [
      "application/json",
      INVOICES_JSON,
      { path: "invoices.1.number", pathType: "json_dot_notation" as const },
    ],
    [
      "application/xml",
      INVOICES_XML,
      { path: "/invoices[1]/invoice[2]/number[1]", pathType: "xml_xpath" as const },
    ],
  ])(
    "opens a citation into a %s tree at the node it quoted",
    async (mediaType, body, structuredPath) => {
      seedTranscript({ id: "cite-tree", structuredPath });
      await openCitation(
        detail({ media_type: mediaType, title: "Invoices", content: body }),
        "cite-tree",
        body,
      );

      const cited = await screen.findByText(/B-2/);
      expect(scrolledTo).toContain(cited.closest("div"));
    },
  );

  // A workbook is opened at a range on a sheet, which is what a citation into
  // one records. A workbook format the panel has no grid viewer for keeps the
  // range on the citation and lands on the extracted text, where the rows it
  // quoted are highlighted instead.
  const CELL_RANGE = {
    startCell: "B2",
    endCell: "D10",
    sheetIndex: 0,
    sheetName: "Q4 Results",
  } as const;
  const SHEET_TEXT = "## Q4 Results\n\n| North | 1204.5 |\n";

  it("opens a citation into a workbook at the range it quoted", async () => {
    seedTranscript({ id: "cite-cells", cellRange: CELL_RANGE });
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
      cellRange: CELL_RANGE,
      span: { start: 3, end: 13 },
    });
    await openCitation(
      detail({
        media_type: "application/vnd.oasis.opendocument.spreadsheet",
        title: "Q4.ods",
        content: SHEET_TEXT,
      }),
      "cite-ods",
    );

    expect(await screen.findByText("Q4 Results")).toBeVisible();
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

  // The passage sits behind an accented character, so the byte offsets a
  // citation carries no longer line up with JavaScript's string indices. An
  // off-by-one conversion highlights the wrong words and this is what catches it.
  const CITED_CONTENT = "Café notes\n\nRevenue rose 12% in the second quarter.\n";
  const CITED_PASSAGE = "Revenue rose 12%";

  it("opens a citation on the passage it quoted, highlighted and scrolled to", async () => {
    seedTranscript({ id: "cite-1", span: byteSpan(CITED_CONTENT, CITED_PASSAGE) });
    await openCitation(
      // A source whose original view could be drawn, but cannot show a span:
      // the citation lands on the extracted text, where the passage is.
      detail({ media_type: "text/plain", title: "Notes.txt", content: CITED_CONTENT }),
      "cite-1",
    );

    const cited = await screen.findByText(CITED_PASSAGE);
    expect(cited.tagName).toBe("MARK");
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

  // A text source is its own text of record — nothing is read out of it — so the
  // citation's offsets address the original as well as the extracted text, and
  // the mark follows the reader from one view to the other.
  //
  // The passage sits behind an accent and behind a single newline the renderer
  // turns into a hard break, which are the two ways a source offset stops
  // agreeing with the position it is drawn at: the first shifts the byte
  // conversion, the second shifts the offsets the parser recorded.
  const MARKDOWN_CONTENT =
    "# Café notes\n\nQuarter one was flat.\nRevenue rose **12%** in the second quarter.\n";
  const MARKDOWN_PASSAGE = "Revenue rose **12%** in the second";

  it("marks the cited passage in a rendered markdown original", async () => {
    const user = userEvent.setup();
    seedTranscript({
      id: "cite-3",
      span: byteSpan(MARKDOWN_CONTENT, MARKDOWN_PASSAGE),
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
    const marks = Array.from(document.querySelectorAll("mark"));
    expect(marks.map((mark) => mark.textContent).join("")).toBe(
      "Revenue rose 12% in the second",
    );
    expect(document.querySelector("strong mark")?.textContent).toBe("12%");
    expect(scrolledTo).toContain(marks[0]);
  });

  it("marks the cited passage in an original drawn as plain text", async () => {
    const user = userEvent.setup();
    seedTranscript({ id: "cite-4", span: byteSpan(CITED_CONTENT, CITED_PASSAGE) });
    await openCitation(
      detail({ media_type: "text/plain", title: "Notes.txt", content: CITED_CONTENT }),
      "cite-4",
      CITED_CONTENT,
    );

    await user.click(await screen.findByRole("tab", { name: "Original document" }));

    const marks = await screen.findAllByText(CITED_PASSAGE);
    expect(marks.every((mark) => mark.tagName === "MARK")).toBe(true);
    // Drawn as written, the file still reads as one run around the mark.
    expect(document.querySelector("pre")?.textContent).toBe(CITED_CONTENT);
  });

  it("opens a paginated citation on its recorded page, then lets the reader leave", async () => {
    const user = userEvent.setup();
    seedTranscript({ id: "cite-2", pages: [4, 5], span: { start: 10, end: 20 } });
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
    seedTranscript({ id: "cite-2", pages: [4, 5], span: { start: 10, end: 20 } });
    expect(await screen.findByText("Page 5")).toBeVisible();

    // A citation that knows only which pages it came from marks none of them,
    // which is every source imported before regions were recorded.
    expect(drawnHighlights()).toHaveLength(0);
  });

  it("marks where a citation sits on the page it is open at, and only there", async () => {
    const user = userEvent.setup();
    seedTranscript({
      id: "cite-5",
      pages: [4, 5],
      bounds: [
        rect(4, { left: 1_000, top: 2_000, width: 8_000, height: 400 }),
        rect(4, { left: 1_000, top: 2_500, width: 6_000, height: 400 }),
        rect(5, { left: 1_000, top: 500, width: 3_000, height: 400 }),
      ],
    });
    await openCitation(
      detail({ media_type: "application/pdf", title: "Report.pdf", content: "text" }),
      "cite-5",
    );

    expect(await screen.findByText("Page 4")).toBeVisible();
    await waitFor(() => expect(drawnHighlights()).toHaveLength(2));
    // Placed as a fraction of the page rather than in pixels, so the boxes
    // survive zooming and resizing without being recomputed.
    expect(drawnHighlights()[0]).toHaveStyle({ top: "calc(20% - 2px)" });

    await user.click(screen.getByRole("button", { name: "Next page" }));
    expect(await screen.findByText("Page 5")).toBeVisible();
    await waitFor(() => expect(drawnHighlights()).toHaveLength(1));
  });

  // The panel is already open at a citation when the next one is clicked, and
  // two citations into one source usually want the same view — so the view a
  // citation asks for cannot tell the second click from the first. A reader who
  // had switched views in between used to stay where they were, and the second
  // citation landed nowhere.
  it("lands a second citation into a source the reader had switched views on", async () => {
    const user = userEvent.setup();
    seedTranscript(
      { id: "cite-6", pages: [2], span: { start: 0, end: 4 } },
      { id: "cite-7", pages: [7], span: { start: 5, end: 9 } },
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

  // A citation can outlive the reading of the source it points into — the file
  // is re-read on import and the parse fails the second time. There is no
  // original view behind the tab then, and sending the reader to it left the
  // panel drawing nothing at all.
  it("says a source failed rather than opening a citation on an empty panel", async () => {
    seedTranscript({ id: "cite-8", pages: [3], span: { start: 0, end: 4 } });
    await openCitation(
      detail({
        media_type: "application/pdf",
        title: "Report.pdf",
        processing_status: "failed",
        content: "",
      }),
      "cite-8",
    );

    expect(await screen.findByText("Failed to process document")).toBeVisible();
  });

  // The panel unmounts the viewer whenever the reader switches views, so
  // without the byte cache every flip back pulls the whole file over again.
  it("draws the original from cache when the reader flips views and back", async () => {
    const user = userEvent.setup();
    const { client } = await openPanel(
      detail({
        media_type: "text/markdown",
        title: "Report.md",
        content: "The text of record.",
        searchable: true,
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
        searchable: true,
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
