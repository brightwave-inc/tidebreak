import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import {
  ConverterMissingError,
  convertPresentationToPdf,
  presentationPdfSource,
} from "./officePdf";
import type { FileBytesSource } from "./useFileDownload";

const PPTX =
  "application/vnd.openxmlformats-officedocument.presentationml.presentation";

beforeEach(() => invokeMock.mockReset());

describe("presentation-to-PDF conversion", () => {
  it("round-trips bytes through the host command as base64", async () => {
    invokeMock.mockResolvedValue({
      status: "converted",
      pdfBase64: btoa("%PDF-1.7 fake"),
    });

    const pdf = await convertPresentationToPdf(
      new Uint8Array([0x50, 0x4b, 0x03, 0x04]),
      PPTX,
    );

    expect(new TextDecoder().decode(pdf)).toBe("%PDF-1.7 fake");
    expect(invokeMock).toHaveBeenCalledWith("convert_presentation_to_pdf", {
      request: { contentBase64: btoa("PK\x03\x04"), mediaType: PPTX },
    });
  });

  it("surfaces a missing converter as its own error, not a failure", async () => {
    invokeMock.mockResolvedValue({ status: "converterMissing" });

    await expect(
      convertPresentationToPdf(new Uint8Array([1]), PPTX),
    ).rejects.toBeInstanceOf(ConverterMissingError);
  });

  it("derives the converted source from the original's immutable cache key", async () => {
    invokeMock.mockResolvedValue({
      status: "converted",
      pdfBase64: btoa("%PDF-1.7 converted"),
    });
    const original: FileBytesSource = {
      id: "doc-1",
      cacheKey: "document/chat/doc-1",
      fetch: async () => ({
        bytes: new Uint8Array([0x50, 0x4b]),
        contentType: PPTX,
      }),
    };

    const converted = presentationPdfSource(original, PPTX);
    expect(converted.cacheKey).toBe("document/chat/doc-1/converted-pdf");

    const fetched = await converted.fetch(new AbortController().signal);
    expect(fetched.contentType).toBe("application/pdf");
    expect(new TextDecoder().decode(fetched.bytes)).toBe("%PDF-1.7 converted");
  });
});
