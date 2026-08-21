import { invoke } from "@tauri-apps/api/core";

export type ImportedDocument = {
  documentId: string;
  displayName: string;
  mediaType: string;
  byteLen: number;
};

export type LibraryImportBatch = {
  results: LibraryImportResult[];
};

export type LibraryImportSuccess = {
  status: "imported" | "already_present";
  document: ImportedDocument;
};

export type LibraryImportResult =
  | LibraryImportSuccess
  | { status: "failed"; displayName: string; message: string };

/**
 * Write one attached file's original bytes wherever the reader chooses.
 *
 * The bytes never pass through the renderer: the host reads them and writes
 * the destination selected in the native save dialog.
 */
export async function exportLibraryDocument(
  chatId: string,
  documentId: string,
): Promise<boolean> {
  return (await invoke("export_library_document", {
    request: { chatId, documentId },
  })) as boolean;
}

export function parseLibraryImportBatch(
  value: unknown,
): LibraryImportBatch | null {
  if (value === null) return null;
  if (
    !isExactRecord(value, ["results"]) ||
    !Array.isArray(value.results) ||
    value.results.length > 1_000
  ) {
    throw new Error("Invalid document import response");
  }
  return {
    results: value.results.map((result) => {
      if (
        typeof result !== "object" ||
        result === null ||
        Array.isArray(result)
      ) {
        throw new Error("Invalid document import response");
      }
      const record = result as Record<string, unknown>;
      if (
        (record.status === "imported" || record.status === "already_present") &&
        isExactRecord(record, [
          "status",
          "documentId",
          "displayName",
          "mediaType",
          "byteLen",
        ])
      ) {
        const document = parseImportedDocument({
          documentId: record.documentId,
          displayName: record.displayName,
          mediaType: record.mediaType,
          byteLen: record.byteLen,
        });
        if (!document) throw new Error("Invalid document import response");
        return { status: record.status, document };
      }
      if (
        record.status === "failed" &&
        isExactRecord(record, ["status", "displayName", "message"]) &&
        typeof record.displayName === "string" &&
        record.displayName.length > 0 &&
        isSafeRendererText(record.displayName, 255) &&
        typeof record.message === "string" &&
        isSafeRendererText(record.message, 500)
      ) {
        return {
          status: "failed",
          displayName: record.displayName,
          message: record.message,
        };
      }
      throw new Error("Invalid document import response");
    }),
  };
}

export function parseImportedDocument(value: unknown): ImportedDocument | null {
  if (value === null) return null;
  if (
    !isExactRecord(value, [
      "documentId",
      "displayName",
      "mediaType",
      "byteLen",
    ]) ||
    !isUuid(value.documentId) ||
    typeof value.displayName !== "string" ||
    value.displayName.length === 0 ||
    !isSafeRendererText(value.displayName, 255) ||
    typeof value.mediaType !== "string" ||
    value.mediaType.length === 0 ||
    value.mediaType.length > 255 ||
    typeof value.byteLen !== "number" ||
    !Number.isSafeInteger(value.byteLen) ||
    value.byteLen < 1 ||
    value.byteLen > MAX_SOURCE_BYTES
  ) {
    throw new Error("Invalid document import response");
  }
  return {
    documentId: value.documentId,
    displayName: value.displayName,
    mediaType: value.mediaType,
    byteLen: value.byteLen,
  };
}

function isUuid(value: unknown): value is string {
  return (
    typeof value === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
      value,
    )
  );
}

function isSafeRendererText(value: string, maxCodePoints: number): boolean {
  const characters = [...value];
  return (
    characters.length <= maxCodePoints &&
    characters.every(
      (character) => !DISALLOWED_RENDERER_CATEGORY.test(character),
    )
  );
}

const DISALLOWED_RENDERER_CATEGORY = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/u;
const MAX_SOURCE_BYTES = 16 * 1024 * 1024;

function isExactRecord(
  value: unknown,
  keys: readonly string[],
): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  const actual = Object.keys(value);
  return (
    actual.length === keys.length && actual.every((key) => keys.includes(key))
  );
}
