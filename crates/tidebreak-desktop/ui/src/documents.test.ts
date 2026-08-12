import { describe, expect, it } from "vitest";

import {
  parseImportedDocument,
  parseLibraryImportBatch,
} from "./documents";

const documentId = "6c3df6af-bc62-4a66-a34e-29f327eaef41";
const document = {
  documentId,
  displayName: "notes.md",
  mediaType: "text/markdown",
  byteLen: 42,
};

describe("file attachment import projection", () => {
  it("parses the identity and renderer-safe chip metadata", () => {
    expect(
      parseLibraryImportBatch({
        results: [{ status: "imported", ...document }],
      }),
    ).toEqual({
      results: [{ status: "imported", document }],
    });
    expect(parseImportedDocument(document)).toEqual(document);
    expect(parseImportedDocument(null)).toBeNull();
  });

  it("rejects malformed, oversized, and expanded host projections", () => {
    expect(() =>
      parseImportedDocument({ ...document, documentId: "not-a-document-id" }),
    ).toThrow("Invalid document import response");
    expect(() =>
      parseImportedDocument({ ...document, byteLen: 16 * 1024 * 1024 + 1 }),
    ).toThrow("Invalid document import response");
    expect(() =>
      parseImportedDocument({ ...document, sourcePath: "/private/notes.md" }),
    ).toThrow("Invalid document import response");
  });
});
