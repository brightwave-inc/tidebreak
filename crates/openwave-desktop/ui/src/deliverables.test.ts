import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import {
  exportDeliverable,
  parseDeliverablePreview,
  parseDeliverablesCatalog,
  parseOutputExportResult,
} from "./deliverables";

const outputId = "550062d4-2528-5cc6-90f8-a788e119bf36";
const revisionId = "72cb0277-5a3c-45ee-bda8-43534f74feb2";

beforeEach(() => invokeMock.mockReset());

describe("deliverable renderer projections", () => {
  it("accepts only the bounded opaque catalog", () => {
    expect(
      parseDeliverablesCatalog({
        deliverables: [
          {
            outputId,
            filename: "Research brief.md",
            mediaType: "text/markdown",
            sizeBytes: 42,
            revisionCount: 2,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
    ).toEqual({
      deliverables: [
        {
          outputId,
          filename: "Research brief.md",
          mediaType: "text/markdown",
          sizeBytes: 42,
          revisionCount: 2,
          updatedAt: "2026-07-24T00:00:00Z",
        },
      ],
      truncated: false,
    });
  });

  it.each([
    "../escape.md",
    "/tmp/private.md",
    ".hidden.md",
    "report.pdf",
    "bad\u{202e}.md",
  ])("rejects unsafe filename %s", (filename) => {
    expect(() =>
      parseDeliverablesCatalog({
        deliverables: [
          {
            outputId,
            filename,
            mediaType: "text/markdown",
            sizeBytes: 1,
            revisionCount: 1,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
    ).toThrow("Invalid output response");
  });

  it("rejects canonical paths, missing opaque ids, and oversized previews", () => {
    expect(() =>
      parseDeliverablePreview({
        outputId,
        filename: "brief.md",
        mediaType: "text/markdown",
        content: "safe",
        truncated: false,
        path: "/private/scratch/brief.md",
      }),
    ).toThrow("Invalid output preview response");
    expect(() =>
      parseDeliverablePreview({
        filename: "brief.md",
        mediaType: "text/markdown",
        content: "safe",
        truncated: false,
      }),
    ).toThrow("Invalid output preview response");
    expect(() =>
      parseDeliverablePreview({
        outputId,
        filename: "brief.md",
        mediaType: "text/markdown",
        content: "x".repeat(100_001),
        truncated: true,
      }),
    ).toThrow("Invalid output preview response");
  });

  it("accepts only exact pathless export receipts", () => {
    expect(
      parseOutputExportResult({
        operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
        outputId,
        revisionId,
        status: "failed",
        reason: "ambiguous_native_failure",
      }),
    ).toEqual({
      operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
      outputId,
      revisionId,
      status: "failed",
      reason: "ambiguous_native_failure",
    });
    expect(() =>
      parseOutputExportResult({
        operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
        outputId,
        revisionId,
        status: "completed",
        destination: "/private/export.md",
      }),
    ).toThrow("Invalid output export response");
  });

  it("retries an ambiguous bridge response with the exact operation identity", async () => {
    invokeMock
      .mockRejectedValueOnce(new Error("bridge response lost"))
      .mockImplementationOnce(
        (
          _command: string,
          {
            request,
          }: {
            request: {
              operationId: string;
              outputId: string;
            };
          },
        ) =>
          Promise.resolve({
            operationId: request.operationId,
            outputId: request.outputId,
            revisionId,
            status: "completed",
          }),
      );

    await expect(exportDeliverable("chat-1", outputId)).resolves.toMatchObject({
      outputId,
      revisionId,
      status: "completed",
    });
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock.mock.calls[1]?.[1]).toEqual(invokeMock.mock.calls[0]?.[1]);
  });
});
