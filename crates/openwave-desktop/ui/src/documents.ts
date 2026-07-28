import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type DocumentProcessingStatus =
  | "queued"
  | "processing"
  | "ready"
  | "failed";

export type LibraryDocument = {
  documentId: string;
  title: string | null;
  mediaType: string;
  processingStatus: DocumentProcessingStatus;
  /** Whether searching this conversation can actually match this source. */
  searchable: boolean;
  updatedAt: string;
};

export type ImportedDocument = {
  documentId: string;
  displayName: string;
  processingStatus: DocumentProcessingStatus;
};

export type LibraryImportState =
  | "queued"
  | "streaming"
  | "imported"
  | "already_present"
  | "failed";

export type LibraryImportProgress = {
  importId: string;
  displayName: string;
  status: LibraryImportState;
  documentId: string | null;
  processingStatus: DocumentProcessingStatus | null;
  message: string | null;
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

export type LibrarySearchResult = {
  documentId: string;
  snippet: string;
  heading: string | null;
};

export type LibraryCatalog = {
  documents: LibraryDocument[];
  truncated: boolean;
};

export async function listLibraryDocuments(chatId: string): Promise<LibraryCatalog> {
  return parseLibraryCatalog(
    await invoke("list_library_documents", { request: { chatId } }),
  );
}

export async function importLibraryDocument(
  chatId: string,
): Promise<ImportedDocument | null> {
  return parseImportedDocument(
    await invoke("import_library_document", { request: { chatId } }),
  );
}

/** Select several sources in one native picker and receive progress by event. */
export async function importLibraryDocuments(
  chatId: string,
): Promise<LibraryImportBatch | null> {
  return parseLibraryImportBatch(
    await invoke("import_library_documents", { request: { chatId } }),
  );
}

/** Claim one just-dropped native file set without ever serializing its paths. */
export async function importDroppedLibraryDocuments(
  chatId: string,
): Promise<LibraryImportBatch | null> {
  return parseLibraryImportBatch(
    await invoke("import_dropped_library_documents", { request: { chatId } }),
  );
}

export async function listenForLibraryImportProgress(
  listener: (progress: LibraryImportProgress) => void,
): Promise<() => void> {
  return listen<unknown>("library-import-progress", (event) => {
    listener(parseLibraryImportProgress(event.payload));
  });
}

export function parseLibraryImportProgress(value: unknown): LibraryImportProgress {
  if (
    !isExactRecord(value, [
      "importId",
      "displayName",
      "status",
      "documentId",
      "processingStatus",
      "message",
    ]) ||
    !isUuid(value.importId) ||
    typeof value.displayName !== "string" ||
    value.displayName.length === 0 ||
    !isSafeRendererText(value.displayName, 255, false) ||
    !isLibraryImportState(value.status) ||
    (value.documentId !== null && !isUuid(value.documentId)) ||
    (value.processingStatus !== null && !isProcessingStatus(value.processingStatus)) ||
    (value.message !== null &&
      (typeof value.message !== "string" ||
        !isSafeRendererText(value.message, 500, false)))
  ) {
    throw new Error("Invalid document import progress");
  }
  return {
    importId: value.importId,
    displayName: value.displayName,
    status: value.status,
    documentId: value.documentId,
    processingStatus: value.processingStatus,
    message: value.message,
  };
}

export function parseLibraryImportBatch(value: unknown): LibraryImportBatch | null {
  if (value === null) return null;
  if (!isExactRecord(value, ["results"]) || !Array.isArray(value.results)) {
    throw new Error("Invalid document import response");
  }
  return {
    results: value.results.map((result) => {
      if (typeof result !== "object" || result === null || Array.isArray(result)) {
        throw new Error("Invalid document import response");
      }
      const record = result as Record<string, unknown>;
      if (
        (record.status === "imported" || record.status === "already_present") &&
        isExactRecord(record, ["status", "documentId", "displayName", "processingStatus"])
      ) {
        const document = parseImportedDocument({
          documentId: record.documentId,
          displayName: record.displayName,
          processingStatus: record.processingStatus,
        });
        if (!document) throw new Error("Invalid document import response");
        return {
          status: record.status,
          document,
        };
      }
      if (
        record.status === "failed" &&
        isExactRecord(record, ["status", "displayName", "message"]) &&
        typeof record.displayName === "string" &&
        record.displayName.length > 0 &&
        isSafeRendererText(record.displayName, 255, false) &&
        typeof record.message === "string" &&
        isSafeRendererText(record.message, 500, false)
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

export async function searchLibraryDocuments(
  chatId: string,
  query: string,
): Promise<LibrarySearchResult[]> {
  return parseLibrarySearchResults(
    await invoke("search_library_documents", { request: { chatId, query } }),
  );
}

export function parseLibraryCatalog(value: unknown): LibraryCatalog {
  if (!isExactRecord(value, ["documents", "truncated"])) {
    throw new Error("Invalid document library response");
  }
  if (!Array.isArray(value.documents) || value.documents.length > 1_000) {
    throw new Error("Invalid document library response");
  }
  if (typeof value.truncated !== "boolean") {
    throw new Error("Invalid document library response");
  }
  return {
    documents: value.documents.map(parseLibraryDocument),
    truncated: value.truncated,
  };
}

export function parseImportedDocument(value: unknown): ImportedDocument | null {
  if (value === null) return null;
  if (!isExactRecord(value, ["documentId", "displayName", "processingStatus"])) {
    throw new Error("Invalid document import response");
  }
  if (
    !isUuid(value.documentId) ||
    typeof value.displayName !== "string" ||
    value.displayName.length === 0 ||
    !isSafeRendererText(value.displayName, 255, false) ||
    !isProcessingStatus(value.processingStatus)
  ) {
    throw new Error("Invalid document import response");
  }
  return {
    documentId: value.documentId,
    displayName: value.displayName,
    processingStatus: value.processingStatus,
  };
}

export function parseLibrarySearchResults(value: unknown): LibrarySearchResult[] {
  if (!Array.isArray(value) || value.length > 8) {
    throw new Error("Invalid document search response");
  }
  return value.map((item) => {
    if (!isExactRecord(item, ["documentId", "snippet", "heading"])) {
      throw new Error("Invalid document search response");
    }
    if (
      !isUuid(item.documentId) ||
      typeof item.snippet !== "string" ||
      !isSafeRendererText(item.snippet, 4_000, true) ||
      (item.heading !== null &&
        (typeof item.heading !== "string" ||
          !isSafeRendererText(item.heading, 200, false)))
    ) {
      throw new Error("Invalid document search response");
    }
    return {
      documentId: item.documentId,
      snippet: item.snippet,
      heading: item.heading,
    };
  });
}

function parseLibraryDocument(value: unknown): LibraryDocument {
  if (
    !isExactRecord(value, [
      "documentId",
      "title",
      "mediaType",
      "processingStatus",
      "searchable",
      "updatedAt",
    ])
  ) {
    throw new Error("Invalid document library response");
  }
  if (
    !isUuid(value.documentId) ||
    (value.title !== null &&
      (typeof value.title !== "string" ||
        !isSafeRendererText(value.title, 255, false))) ||
    typeof value.mediaType !== "string" ||
    value.mediaType.length === 0 ||
    value.mediaType.length > 255 ||
    !isProcessingStatus(value.processingStatus) ||
    typeof value.searchable !== "boolean" ||
    typeof value.updatedAt !== "string" ||
    !Number.isFinite(Date.parse(value.updatedAt))
  ) {
    throw new Error("Invalid document library response");
  }
  return {
    documentId: value.documentId,
    title: value.title,
    mediaType: value.mediaType,
    processingStatus: value.processingStatus,
    searchable: value.searchable,
    updatedAt: value.updatedAt,
  };
}

function isProcessingStatus(value: unknown): value is DocumentProcessingStatus {
  return ["queued", "processing", "ready", "failed"].includes(String(value));
}

function isLibraryImportState(value: unknown): value is LibraryImportState {
  return ["queued", "streaming", "imported", "already_present", "failed"].includes(
    String(value),
  );
}

function isUuid(value: unknown): value is string {
  return (
    typeof value === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
      value,
    )
  );
}

function isSafeRendererText(
  value: string,
  maxCodePoints: number,
  allowLineBreaks: boolean,
): boolean {
  const characters = [...value];
  if (characters.length > maxCodePoints) return false;
  return characters.every((character) => {
    if (allowLineBreaks && ["\n", "\r", "\t"].includes(character)) return true;
    return !DISALLOWED_RENDERER_CATEGORY.test(character);
  });
}

const DISALLOWED_RENDERER_CATEGORY = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/u;

function isExactRecord(
  value: unknown,
  keys: readonly string[],
): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}
