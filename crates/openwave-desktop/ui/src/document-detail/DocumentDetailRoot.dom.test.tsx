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

async function openPanel(info: DocumentDetail, download = vi.fn()) {
  const client = {
    getChatDocument: vi.fn().mockResolvedValue(info),
    getChatDocumentFile: vi.fn().mockResolvedValue(new Blob(["bytes"])),
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
    expect(client.getChatDocumentFile).toHaveBeenCalledWith(
      "chat-1",
      "doc-1",
      expect.anything(),
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
});
