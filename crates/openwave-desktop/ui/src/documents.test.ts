import { describe, expect, it } from "vitest";
import {
  parseImportedDocument,
  parseLibraryImportBatch,
  parseLibraryImportProgress,
  parseLibraryCatalog,
  parseLibrarySearchResults,
} from "./documents";

const documentId = "6c3df6af-bc62-4a66-a34e-29f327eaef41";

function catalogRow(overrides: Record<string, unknown> = {}) {
  return {
    documentId,
    title: "notes.md",
    mediaType: "text/markdown",
    sizeBytes: 2048,
    processingStatus: "ready",
    failure: null,
    searchable: true,
    createdAt: "2026-07-17T12:00:00Z",
    updatedAt: "2026-07-18T12:00:00Z",
    ...overrides,
  };
}

describe("document library renderer projections", () => {
  it("parses native batch results and rejects invalid progress", () => {
    expect(
      parseLibraryImportBatch({
        results: [
          {
            status: "imported",
            documentId,
            displayName: "notes.md",
            processingStatus: "queued",
          },
        ],
      }),
    ).toEqual({
      results: [
        {
          status: "imported",
          document: {
            documentId,
            displayName: "notes.md",
            processingStatus: "queued",
          },
        },
      ],
    });

    expect(() =>
      parseLibraryImportProgress({
        importId: "not-an-id",
        displayName: "notes.md",
        status: "streaming",
        documentId: null,
        processingStatus: null,
        message: null,
      }),
    ).toThrow("Invalid document import progress");
  });

  it("accepts the closed catalog and import shapes", () => {
    expect(
      parseLibraryCatalog({
        documents: [catalogRow({ processingStatus: "processing", searchable: false })],
        truncated: false,
      }).documents,
    ).toHaveLength(1);

    expect(
      parseImportedDocument({
        documentId,
        displayName: "notes.md",
        processingStatus: "queued",
      }),
    ).toEqual({
      documentId,
      displayName: "notes.md",
      processingStatus: "queued",
    });
    expect(parseImportedDocument(null)).toBeNull();
    const unicodeName = `${"😀".repeat(250)}.md`;
    expect(
      parseImportedDocument({
        documentId,
        displayName: unicodeName,
        processingStatus: "queued",
      })?.displayName,
    ).toBe(unicodeName);
  });

  it("keeps searchability distinct from having finished processing", () => {
    const stored = parseLibraryCatalog({
      documents: [
        catalogRow({
          title: "scan.pdf",
          mediaType: "application/pdf",
          searchable: false,
        }),
      ],
      truncated: false,
    }).documents[0];
    expect(stored?.processingStatus).toBe("ready");
    expect(stored?.searchable).toBe(false);

    // Omitting the flag must not silently read as searchable.
    const { searchable: _searchable, ...withoutFlag } = catalogRow();
    expect(() =>
      parseLibraryCatalog({ documents: [withoutFlag], truncated: false }),
    ).toThrow("Invalid document library response");
  });

  it("carries a failure reason only as a category the reader can be told about", () => {
    const failed = parseLibraryCatalog({
      documents: [
        catalogRow({
          processingStatus: "failed",
          searchable: false,
          failure: { reason: "parse_failed", retriable: false },
        }),
      ],
      truncated: false,
    }).documents[0];
    expect(failed?.failure).toEqual({ reason: "parse_failed", retriable: false });

    // Free text would end up rendered verbatim, and a missing verdict would
    // have to be guessed at. Neither is allowed through.
    for (const failure of [
      { reason: "Parse failed: /Users/private/notes.md", retriable: false },
      { reason: "parse_failed" },
    ]) {
      expect(() =>
        parseLibraryCatalog({
          documents: [catalogRow({ processingStatus: "failed", failure })],
          truncated: false,
        }),
      ).toThrow("Invalid document library response");
    }
  });

  it("rejects catalog fields that could reveal native or indexing details", () => {
    for (const field of [
      "uri",
      "sourcePath",
      "contentRevision",
      "indexedRevision",
      "indexFingerprint",
      "generationToken",
      "content",
    ]) {
      expect(() =>
        parseLibraryCatalog({
          documents: [catalogRow({ [field]: "private" })],
          truncated: false,
        }),
      ).toThrow("Invalid document library response");
    }
  });

  it("accepts plain search passages and rejects canonical result metadata", () => {
    expect(
      parseLibrarySearchResults([
        { documentId, snippet: "A matching passage", heading: "Overview" },
      ]),
    ).toEqual([
      { documentId, snippet: "A matching passage", heading: "Overview" },
    ]);

    expect(() =>
      parseLibrarySearchResults([
        {
          documentId,
          snippet: "A matching passage",
          heading: null,
          chunkId: "private",
          score: 0.9,
          sourceRegions: [{ path: "/private/notes.md" }],
        },
      ]),
    ).toThrow("Invalid document search response");
  });

  it("rejects malformed, oversized, and unbounded projections", () => {
    expect(() =>
      parseImportedDocument({
        documentId: "not-a-document-id",
        displayName: "notes.md",
        processingStatus: "ready",
      }),
    ).toThrow("Invalid document import response");
    for (const unsafe of ["\u200d", "\u206a", "\u206f", "\u2028", "\u2029"]) {
      expect(() =>
        parseImportedDocument({
          documentId,
          displayName: `report${unsafe}.md`,
          processingStatus: "ready",
        }),
      ).toThrow("Invalid document import response");
      expect(() =>
        parseLibrarySearchResults([
          { documentId, snippet: `unsafe${unsafe}`, heading: null },
        ]),
      ).toThrow("Invalid document search response");
    }
    expect(() =>
      parseImportedDocument({
        documentId,
        displayName: "report\u202etxt.md",
        processingStatus: "ready",
      }),
    ).toThrow("Invalid document import response");
    expect(() =>
      parseLibrarySearchResults([
        { documentId, snippet: "x".repeat(4_001), heading: null },
      ]),
    ).toThrow("Invalid document search response");
    expect(() =>
      parseLibrarySearchResults(
        Array.from({ length: 9 }, () => ({ documentId, snippet: "x", heading: null })),
      ),
    ).toThrow("Invalid document search response");
  });
});
