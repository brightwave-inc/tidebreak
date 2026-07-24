import { describe, expect, it } from "vitest";
import {
  parseImportedDocument,
  parseLibraryCatalog,
  parseLibrarySearchResults,
} from "./documents";

const documentId = "6c3df6af-bc62-4a66-a34e-29f327eaef41";

describe("document library renderer projections", () => {
  it("accepts the closed catalog and import shapes", () => {
    expect(
      parseLibraryCatalog({
        documents: [
          {
            documentId,
            title: "notes.md",
            mediaType: "text/markdown",
            processingStatus: "processing",
            searchable: false,
            updatedAt: "2026-07-18T12:00:00Z",
          },
        ],
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
        {
          documentId,
          title: "scan.pdf",
          mediaType: "application/pdf",
          processingStatus: "ready",
          searchable: false,
          updatedAt: "2026-07-18T12:00:00Z",
        },
      ],
      truncated: false,
    }).documents[0];
    expect(stored?.processingStatus).toBe("ready");
    expect(stored?.searchable).toBe(false);

    // Omitting the flag must not silently read as searchable.
    expect(() =>
      parseLibraryCatalog({
        documents: [
          {
            documentId,
            title: "scan.pdf",
            mediaType: "application/pdf",
            processingStatus: "ready",
            updatedAt: "2026-07-18T12:00:00Z",
          },
        ],
        truncated: false,
      }),
    ).toThrow("Invalid document library response");
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
          documents: [
            {
              documentId,
              title: "notes.md",
              mediaType: "text/markdown",
              processingStatus: "ready",
              searchable: true,
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
