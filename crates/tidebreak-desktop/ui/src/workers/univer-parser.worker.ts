import type { IWorkbookData } from "@univerjs/presets";
import * as XLSX from "xlsx";

import { sheetjsToUniver } from "@/document/sheetjs-to-univer";
import { extractXlsxMetadata } from "@/document/xlsx-styles-parser";

// LRU cache for parsed workbook data (keyed by documentId)
const MAX_CACHE_SIZE = 5;
const workbookCache = new Map<string, { workbookData: IWorkbookData }>();

function lruGet<K, V>(cache: Map<K, V>, key: K): V | undefined {
  const value = cache.get(key);
  if (value !== undefined) {
    cache.delete(key);
    cache.set(key, value);
  }
  return value;
}

function lruSet<K, V>(cache: Map<K, V>, key: K, value: V, maxSize: number) {
  if (cache.size >= maxSize && !cache.has(key)) {
    const firstKey = cache.keys().next().value;
    if (firstKey !== undefined) {
      cache.delete(firstKey);
    }
  }
  cache.delete(key);
  cache.set(key, value);
}

// ── Message types ────────────────────────────────────────────────────────────

interface ParseMessage {
  type: "parse";
  data: ArrayBuffer;
  documentId: string;
  opId: string;
  isCsv?: boolean;
}

interface ClearCacheMessage {
  type: "clear-cache";
  documentId: string;
}

interface CacheMissMessage {
  type: "cache-miss";
  opId: string;
}

interface ResultMessage {
  type: "result";
  workbookData: IWorkbookData;
  opId: string;
}

interface ErrorMessage {
  type: "error";
  error: string;
  opId: string;
}

interface CacheClearedMessage {
  type: "cache-cleared";
  documentId: string;
}

type WorkerRequest = ParseMessage | ClearCacheMessage;

// ── Message handler ──────────────────────────────────────────────────────────

self.onmessage = async (e: MessageEvent<WorkerRequest>) => {
  if (e.data.type === "clear-cache") {
    const { documentId } = e.data;
    workbookCache.delete(documentId);
    self.postMessage({
      type: "cache-cleared",
      documentId,
    } satisfies CacheClearedMessage);
    return;
  }

  const { opId, documentId, data, isCsv } = e.data;

  try {
    const cached = lruGet(workbookCache, documentId);
    if (cached) {
      self.postMessage({
        type: "result",
        workbookData: cached.workbookData,
        opId,
      } satisfies ResultMessage);
      return;
    }

    self.postMessage({
      type: "cache-miss",
      opId,
    } satisfies CacheMissMessage);

    const workbook = XLSX.read(data, {
      type: "array",
      cellStyles: !isCsv,
      cellFormula: true,
      cellNF: true,
    });

    let workbookData: IWorkbookData;

    if (isCsv) {
      workbookData = sheetjsToUniver(workbook);
    } else {
      const metadata = await extractXlsxMetadata(data);
      workbookData = sheetjsToUniver(
        workbook,
        metadata.fontStyles,
        metadata.freezePanes,
      );
    }

    lruSet(workbookCache, documentId, { workbookData }, MAX_CACHE_SIZE);

    self.postMessage({
      type: "result",
      workbookData,
      opId,
    } satisfies ResultMessage);
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    self.postMessage({
      type: "error",
      error: errorMessage,
      opId,
    } satisfies ErrorMessage);
  }
};
