import { invoke } from "@tauri-apps/api/core";

export type DeliverableSummary = {
  filename: string;
  mediaType: DeliverableMediaType;
  sizeBytes: number;
  updatedAt: string;
};

export type DeliverablesCatalog = {
  deliverables: DeliverableSummary[];
  truncated: boolean;
};

export type DeliverablePreview = {
  filename: string;
  mediaType: DeliverableMediaType;
  content: string;
  truncated: boolean;
};

export type DeliverableMediaType =
  | "text/markdown"
  | "text/plain"
  | "text/csv"
  | "application/json"
  | "text/html";

const MAX_DELIVERABLES = 100;
const MAX_FILENAME_CHARACTERS = 120;
const MAX_PREVIEW_CHARACTERS = 100_000;
const MAX_DELIVERABLE_BYTES = 512 * 1024;

export async function listDeliverables(
  chatId: string,
): Promise<DeliverablesCatalog> {
  return parseDeliverablesCatalog(
    await invoke("list_deliverables", { request: { chatId } }),
  );
}

export async function readDeliverable(
  chatId: string,
  filename: string,
): Promise<DeliverablePreview> {
  return parseDeliverablePreview(
    await invoke("read_deliverable", { request: { chatId, filename } }),
  );
}

export async function exportDeliverable(
  chatId: string,
  filename: string,
): Promise<boolean> {
  const exported = await invoke("export_deliverable", {
    request: { chatId, filename },
  });
  if (typeof exported !== "boolean") {
    throw new Error("Invalid output export response");
  }
  return exported;
}

export function parseDeliverablesCatalog(value: unknown): DeliverablesCatalog {
  if (!isExactRecord(value, ["deliverables", "truncated"])) {
    throw new Error("Invalid outputs response");
  }
  if (
    !Array.isArray(value.deliverables) ||
    value.deliverables.length > MAX_DELIVERABLES ||
    typeof value.truncated !== "boolean"
  ) {
    throw new Error("Invalid outputs response");
  }
  return {
    deliverables: value.deliverables.map(parseDeliverableSummary),
    truncated: value.truncated,
  };
}

export function parseDeliverablePreview(value: unknown): DeliverablePreview {
  if (
    !isExactRecord(value, ["filename", "mediaType", "content", "truncated"]) ||
    !isDeliverableFilename(value.filename) ||
    !isDeliverableMediaType(value.mediaType) ||
    typeof value.content !== "string" ||
    [...value.content].length > MAX_PREVIEW_CHARACTERS ||
    value.content.includes("\0") ||
    typeof value.truncated !== "boolean"
  ) {
    throw new Error("Invalid output preview response");
  }
  return {
    filename: value.filename,
    mediaType: value.mediaType,
    content: value.content,
    truncated: value.truncated,
  };
}

function parseDeliverableSummary(value: unknown): DeliverableSummary {
  if (
    !isExactRecord(value, [
      "filename",
      "mediaType",
      "sizeBytes",
      "updatedAt",
    ]) ||
    !isDeliverableFilename(value.filename) ||
    !isDeliverableMediaType(value.mediaType) ||
    typeof value.sizeBytes !== "number" ||
    !Number.isSafeInteger(value.sizeBytes) ||
    value.sizeBytes < 0 ||
    value.sizeBytes > MAX_DELIVERABLE_BYTES ||
    typeof value.updatedAt !== "string" ||
    !Number.isFinite(Date.parse(value.updatedAt))
  ) {
    throw new Error("Invalid output response");
  }
  return {
    filename: value.filename,
    mediaType: value.mediaType,
    sizeBytes: value.sizeBytes,
    updatedAt: value.updatedAt,
  };
}

function isDeliverableFilename(value: unknown): value is string {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    [...value].length > MAX_FILENAME_CHARACTERS ||
    !/^[A-Za-z0-9][A-Za-z0-9 _().-]*$/.test(value) ||
    value.endsWith(".") ||
    value.trim() !== value
  ) {
    return false;
  }
  return /\.(?:md|txt|csv|json|html)$/i.test(value);
}

function isDeliverableMediaType(value: unknown): value is DeliverableMediaType {
  return [
    "text/markdown",
    "text/plain",
    "text/csv",
    "application/json",
    "text/html",
  ].includes(String(value));
}

function isExactRecord(
  value: unknown,
  keys: readonly string[],
): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}
