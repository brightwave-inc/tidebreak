import { describe, expect, it } from "vitest";
import {
  parseImportedDocument,
  parseLibraryImportBatch,
  parseLibraryImportProgress,
  parseLibraryCatalog,
  parseLibrarySearchResults,
} from "./documents";

const documentId = "6c3df6af-bc62-4a66-a34e-29f327eaef41";
const importId = "11111111-1111-4111-8111-111111111111";

describe("document library renderer projections", () => {
  it("parses the slim import and progress shapes exactly", () => {
    expect(
      parseLibraryImportBatch({
        results: [{ status: "imported", documentId, displayName: "notes.md" }],
      }),
    ).toEqual({
      results: [
        {
          status: "imported",
          document: { documentId, displayName: "notes.md" },
        },
      ],
    });
    expect(
      parseLibraryImportProgress({
        importId,
        displayName: "notes.md",
        status: "imported",
        documentId,
        message: null,
      }),
    ).toEqual({
      importId,
      displayName: "notes.md",
      status: "imported",
      documentId,
      message: null,
    });
    expect(parseImportedDocument({ documentId, displayName: "notes.md" })).toEqual({
      documentId,
      displayName: "notes.md",
    });
    expect(parseImportedDocument(null)).toBeNull();
  });

  it("derives source usability from the required readable flag", () => {
    const stored = parseLibraryCatalog({
      documents: [
        {
          documentId,
          title: "scan.pdf",
          mediaType: "application/pdf",
          sizeBytes: 2_048,
          readable: false,
          updatedAt: "2026-07-18T12:00:00Z",
        },
      ],
      truncated: false,
    }).documents[0];
    expect(stored?.readable).toBe(false);

    expect(() =>
      parseLibraryCatalog({
        documents: [
          {
            documentId,
            title: "scan.pdf",
            mediaType: "application/pdf",
            sizeBytes: 2_048,
            updatedAt: "2026-07-18T12:00:00Z",
          },
        ],
        truncated: false,
      }),
    ).toThrow("Invalid document library response");
  });

  it("rejects native, pipeline, and canonical metadata at the renderer boundary", () => {
    for (const field of [
      "uri",
      "sourcePath",
      "contentRevision",
      "processingStatus",
      "failure",
      "generationToken",
      "content",
    ]) {
      expect(() =>
        parseLibraryCatalog({
          documents: [
            {
              documentId,
              title: "notes.md",
              mediaType: "text/markdown",
              sizeBytes: 42,
              readable: true,
              updatedAt: "2026-07-18T12:00:00Z",
              [field]: "private",
            },
          ],
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
      }),
    ).toThrow("Invalid document import response");
    expect(() =>
      parseLibraryImportBatch({
        results: Array.from({ length: 1_001 }, () => ({
          status: "failed",
          displayName: "entry.md",
          message: "Could not import",
        })),
      }),
    ).toThrow("Invalid document import response");
    expect(() =>
      parseLibrarySearchResults(
        Array.from({ length: 9 }, () => ({ documentId, snippet: "x", heading: null })),
      ),
    ).toThrow("Invalid document search response");
  });
});
