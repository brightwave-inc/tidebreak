// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import type { ApiClient, DocumentDetail } from "@/api";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "../test/router";
import { DocumentDetailRoot } from "./DocumentDetailRoot";

afterEach(cleanup);

beforeAll(() => {
  // jsdom implements neither half of the object-URL API the image viewer uses.
  URL.createObjectURL = vi.fn(() => "blob:document");
  URL.revokeObjectURL = vi.fn();
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
  bytes: Blob = new Blob(["bytes"]),
) {
  const client = {
    getChatDocument: vi.fn().mockResolvedValue(info),
    getChatDocumentFile: vi.fn().mockResolvedValue(bytes),
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
    await openPanel(detail({ media_type: mediaType, title: "Data" }), vi.fn(), new Blob([body]));

    expect(await screen.findByRole("button", { name: "Collapse all" })).toBeVisible();
    // Decomposed into nodes, so the file's own text never appears as one run.
    expect(screen.queryByText(body)).toBeNull();
  });

  // The outline slugs the raw markdown and the renderer slugs the rendered
  // heading, independently. This is the only test that puts the two together,
  // so it is what would catch them drifting apart.
  it("lists a markdown source's headings and scrolls to one", async () => {
    const user = userEvent.setup();
    await openPanel(
      detail({ media_type: "text/markdown", title: "Report.md" }),
      vi.fn(),
      new Blob(["# Quarterly report\n\nBody.\n\n## Revenue by **segment**\n\nMore.\n"]),
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
      new Blob(["# Not a heading, just a line that starts with a hash\n"]),
    );

    expect(
      await screen.findByText(/Not a heading, just a line that starts with a hash/),
    ).toBeVisible();
    expect(screen.queryByRole("button", { name: "Document outline" })).toBeNull();
  });

  it("says so when a structured source will not parse", async () => {
    await openPanel(
      detail({ media_type: "application/json", title: "Truncated.json" }),
      vi.fn(),
      new Blob(['{"invoice": ']),
    );

    expect(await screen.findByText("Unable to parse JSON")).toBeVisible();
  });
});
