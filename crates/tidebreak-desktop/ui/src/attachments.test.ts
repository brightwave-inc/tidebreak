// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";

import type { ApiClient } from "./api";
import { attachHeldChatFiles, pickHeldFiles } from "./attachments";

function png(name = "chart.png"): File {
  return new File([new Uint8Array([1, 2, 3, 4])], name, { type: "image/png" });
}

function pdf(name = "notes.pdf"): File {
  return new File([new Uint8Array([5, 6])], name, { type: "application/pdf" });
}

function clientWith(
  ingestChatDocument: ApiClient["ingestChatDocument"],
): ApiClient {
  return { ingestChatDocument } as ApiClient;
}

describe("attachHeldChatFiles", () => {
  it("posts sources to the machine and leaves images for the composer to upload", async () => {
    const ingest = vi.fn(async () => ({ document_id: "doc-1" }));
    const held = await attachHeldChatFiles(clientWith(ingest), "chat-1", [
      png(),
      pdf(),
    ]);

    // The bytes never reach the host, so the split the host performs on picked
    // paths is performed here instead.
    expect(held.images.map((file) => file.name)).toEqual(["chart.png"]);
    expect(ingest).toHaveBeenCalledWith("chat-1", expect.any(File));
    expect(held.documents?.results).toEqual([
      {
        status: "imported",
        document: {
          documentId: "doc-1",
          displayName: "notes.pdf",
          mediaType: "application/pdf",
          byteLen: 2,
        },
      },
    ]);
  });

  it("declares a file the browser could not type as opaque bytes", async () => {
    const ingest = vi.fn(async () => ({ document_id: "doc-2" }));
    const untyped = new File([new Uint8Array([7])], "notes.md", { type: "" });
    const held = await attachHeldChatFiles(clientWith(ingest), "chat-1", [
      untyped,
    ]);

    expect(held.images).toEqual([]);
    expect(held.documents?.results[0]).toMatchObject({
      status: "imported",
      document: { mediaType: "application/octet-stream" },
    });
  });

  it("reports one refused file beside the ones that landed", async () => {
    const ingest = vi.fn(async (_chatId: string, file: File) => {
      if (file.name === "broken.pdf") throw new Error("Unsupported source");
      return { document_id: "doc-3" };
    });
    const held = await attachHeldChatFiles(clientWith(ingest), "chat-1", [
      pdf("broken.pdf"),
      pdf("good.pdf"),
    ]);

    expect(held.documents?.results).toEqual([
      {
        status: "failed",
        displayName: "broken.pdf",
        message: "Unsupported source",
      },
      {
        status: "imported",
        document: {
          documentId: "doc-3",
          displayName: "good.pdf",
          mediaType: "application/pdf",
          byteLen: 2,
        },
      },
    ]);
  });

  it("posts nothing when the selection is all images", async () => {
    const ingest = vi.fn(async () => ({ document_id: "doc-4" }));
    const held = await attachHeldChatFiles(clientWith(ingest), "chat-1", [
      png(),
    ]);

    expect(ingest).not.toHaveBeenCalled();
    expect(held.documents).toBeNull();
  });
});

describe("pickHeldFiles", () => {
  it("resolves empty when the reader dismisses the picker", async () => {
    const opened = new Promise<HTMLInputElement>((resolve) => {
      vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(
        function (this: HTMLInputElement) {
          resolve(this);
        },
      );
    });
    const picked = pickHeldFiles();
    const input = await opened;
    input.dispatchEvent(new Event("cancel"));

    // A dismissal is not a failure, and nothing upstream should have to tell
    // the two apart.
    await expect(picked).resolves.toEqual([]);
    expect(input.isConnected).toBe(false);
    vi.restoreAllMocks();
  });
});
