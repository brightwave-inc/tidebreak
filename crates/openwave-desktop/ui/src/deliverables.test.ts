import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import {
  exportDeliverable,
  parseDeliverableFile,
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
            producingRunId: null,
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
          producingRunId: null,
        },
      ],
      truncated: false,
    });
  });

  it("carries a submitted output's producing run and rejects a malformed one", () => {
    const runId = "ce116263-15b5-5df2-b472-269378e9da58";
    const [summary] = parseDeliverablesCatalog({
      deliverables: [
        {
          outputId,
          filename: "Q3 revenue.md",
          mediaType: "text/markdown",
          sizeBytes: 42,
          revisionCount: 1,
          updatedAt: "2026-07-24T00:00:00Z",
          producingRunId: runId,
        },
      ],
      truncated: false,
    }).deliverables;
    expect(summary.producingRunId).toBe(runId);
    expect(() =>
      parseDeliverablesCatalog({
        deliverables: [
          {
            outputId,
            filename: "brief.md",
            mediaType: "text/markdown",
            sizeBytes: 42,
            revisionCount: 1,
            updatedAt: "2026-07-24T00:00:00Z",
            producingRunId: "not-a-uuid",
          },
        ],
        truncated: false,
      }),
    ).toThrow("Invalid output response");
  });

  it("accepts a binary artifact at its wider size ceiling and bounds it there", () => {
    const summary = {
      outputId,
      filename: "chart.png",
      mediaType: "image/png",
      sizeBytes: 16 * 1024 * 1024,
      revisionCount: 1,
      updatedAt: "2026-07-24T00:00:00Z",
      producingRunId: null,
    };
    expect(
      parseDeliverablesCatalog({ deliverables: [summary], truncated: false })
        .deliverables[0],
    ).toEqual(summary);
    expect(() =>
      parseDeliverablesCatalog({
        deliverables: [{ ...summary, sizeBytes: 16 * 1024 * 1024 + 1 }],
        truncated: false,
      }),
    ).toThrow("Invalid output response");
    // Curated text keeps its own, smaller ceiling.
    expect(() =>
      parseDeliverablesCatalog({
        deliverables: [
          {
            ...summary,
            filename: "table.csv",
            mediaType: "text/csv",
            sizeBytes: 512 * 1024 + 1,
          },
        ],
        truncated: false,
      }),
    ).toThrow("Invalid output response");
  });

  it.each(["", "noslash", "two/sl/ashes", "text/há", `x/${"y".repeat(127)}`])(
    "rejects malformed media type %s",
    (mediaType) => {
      expect(() =>
        parseDeliverablesCatalog({
          deliverables: [
            {
              outputId,
              filename: "artifact.bin",
              mediaType,
              sizeBytes: 1,
              revisionCount: 1,
              updatedAt: "2026-07-24T00:00:00Z",
              producingRunId: null,
            },
          ],
          truncated: false,
        }),
      ).toThrow("Invalid output response");
    },
  );

  it.each([
    "../escape.md",
    "/tmp/private.md",
    ".hidden.md",
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
            producingRunId: null,
          },
        ],
        truncated: false,
      }),
    ).toThrow("Invalid output response");
  });

  it("rejects canonical paths, missing opaque ids, and oversized previews", () => {
    expect(
      parseDeliverablePreview({
        outputId,
        filename: "brief.md",
        mediaType: "text/markdown",
        revisionCount: 1,
        revisionId,
        content: "safe",
        truncated: false,
      }),
    ).toEqual({
      outputId,
      filename: "brief.md",
      mediaType: "text/markdown",
      revisionCount: 1,
      revisionId,
      content: "safe",
      truncated: false,
    });
    expect(() =>
      parseDeliverablePreview({
        outputId,
        filename: "brief.md",
        mediaType: "text/markdown",
        revisionCount: 1,
        revisionId,
        content: "safe",
        truncated: false,
        path: "/private/scratch/brief.md",
      }),
    ).toThrow("Invalid output preview response");
    expect(() =>
      parseDeliverablePreview({
        filename: "brief.md",
        mediaType: "text/markdown",
        revisionCount: 1,
        revisionId,
        content: "safe",
        truncated: false,
      }),
    ).toThrow("Invalid output preview response");
    expect(() =>
      parseDeliverablePreview({
        outputId,
        filename: "brief.md",
        mediaType: "text/markdown",
        revisionCount: 1,
        revisionId,
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

  it("accepts an output file's bytes and rejects empty or mislabelled ones", () => {
    const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
    expect(
      parseDeliverableFile({
        outputId,
        revisionId,
        mediaType: "image/png",
        bytes,
      }),
    ).toEqual({ outputId, revisionId, mediaType: "image/png", bytes });
    expect(() =>
      parseDeliverableFile({
        outputId,
        revisionId,
        mediaType: "image/png",
        bytes: new Uint8Array(),
      }),
    ).toThrow("Invalid output file response");
    // The content route names the revision it served in a header; a response
    // without one cannot be attributed to immutable bytes.
    expect(() =>
      parseDeliverableFile({
        outputId,
        revisionId: undefined,
        mediaType: "image/png",
        bytes,
      }),
    ).toThrow("Invalid output file response");
  });
});
