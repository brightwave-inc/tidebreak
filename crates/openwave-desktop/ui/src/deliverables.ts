import { invoke } from "@tauri-apps/api/core";

export type DeliverableSummary = {
  outputId: string;
  filename: string;
  mediaType: DeliverableMediaType;
  sizeBytes: number;
  revisionCount: number;
  updatedAt: string;
};

export type DeliverablesCatalog = {
  deliverables: DeliverableSummary[];
  truncated: boolean;
};

export type DeliverablePreview = {
  outputId: string;
  filename: string;
  mediaType: DeliverableMediaType;
  content: string;
  truncated: boolean;
};

export type OutputExportResult =
  | {
      operationId: string;
      outputId: string;
      revisionId: string;
      status: "completed" | "cancelled";
    }
  | {
      operationId: string;
      outputId: string;
      revisionId: string;
      status: "failed";
      reason:
        | "source_unavailable"
        | "destination_unavailable"
        | "ambiguous_native_failure";
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
const MAX_OUTPUT_REVISIONS = 100;

export async function listDeliverables(
  chatId: string,
): Promise<DeliverablesCatalog> {
  return parseDeliverablesCatalog(
    await invoke("list_deliverables", { request: { chatId } }),
  );
}

export async function readDeliverable(
  chatId: string,
  outputId: string,
): Promise<DeliverablePreview> {
  return parseDeliverablePreview(
    await invoke("read_deliverable", { request: { chatId, outputId } }),
  );
}

export async function exportDeliverable(
  chatId: string,
  outputId: string,
): Promise<OutputExportResult> {
  const operationId = crypto.randomUUID();
  const request = { operationId, chatId, outputId };
  let value: unknown;
  try {
    value = await invoke("export_deliverable", { request });
  } catch {
    // The native side persists the operation before opening the picker and
    // never repeats a possibly dispatched write. One exact retry therefore
    // recovers the same terminal receipt after an ambiguous bridge response.
    value = await invoke("export_deliverable", { request });
  }
  return parseOutputExportResult(value, operationId, outputId);
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
    !isExactRecord(value, [
      "outputId",
      "filename",
      "mediaType",
      "content",
      "truncated",
    ]) ||
    !isOpaqueId(value.outputId) ||
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
    outputId: value.outputId,
    filename: value.filename,
    mediaType: value.mediaType,
    content: value.content,
    truncated: value.truncated,
  };
}

export function parseOutputExportResult(
  value: unknown,
  expectedOperationId?: string,
  expectedOutputId?: string,
): OutputExportResult {
  if (
    !isRecord(value) ||
    !isOpaqueId(value.operationId) ||
    !isOpaqueId(value.outputId) ||
    !isOpaqueId(value.revisionId) ||
    (expectedOperationId !== undefined &&
      value.operationId !== expectedOperationId) ||
    (expectedOutputId !== undefined && value.outputId !== expectedOutputId)
  ) {
    throw new Error("Invalid output export response");
  }
  if (
    (value.status === "completed" || value.status === "cancelled") &&
    isExactRecord(value, ["operationId", "outputId", "revisionId", "status"])
  ) {
    return {
      operationId: value.operationId,
      outputId: value.outputId,
      revisionId: value.revisionId,
      status: value.status,
    };
  }
  if (
    value.status === "failed" &&
    isExactRecord(value, [
      "operationId",
      "outputId",
      "revisionId",
      "status",
      "reason",
    ]) &&
    [
      "source_unavailable",
      "destination_unavailable",
      "ambiguous_native_failure",
    ].includes(String(value.reason))
  ) {
    return {
      operationId: value.operationId,
      outputId: value.outputId,
      revisionId: value.revisionId,
      status: "failed",
      reason: value.reason as Extract<
        OutputExportResult,
        { status: "failed" }
      >["reason"],
    };
  }
  throw new Error("Invalid output export response");
}

function parseDeliverableSummary(value: unknown): DeliverableSummary {
  if (
    !isExactRecord(value, [
      "outputId",
      "filename",
      "mediaType",
      "sizeBytes",
      "revisionCount",
      "updatedAt",
    ]) ||
    !isOpaqueId(value.outputId) ||
    !isDeliverableFilename(value.filename) ||
    !isDeliverableMediaType(value.mediaType) ||
    typeof value.sizeBytes !== "number" ||
    !Number.isSafeInteger(value.sizeBytes) ||
    value.sizeBytes < 0 ||
    value.sizeBytes > MAX_DELIVERABLE_BYTES ||
    typeof value.revisionCount !== "number" ||
    !Number.isSafeInteger(value.revisionCount) ||
    value.revisionCount < 1 ||
    value.revisionCount > MAX_OUTPUT_REVISIONS ||
    typeof value.updatedAt !== "string" ||
    !Number.isFinite(Date.parse(value.updatedAt))
  ) {
    throw new Error("Invalid output response");
  }
  return {
    outputId: value.outputId,
    filename: value.filename,
    mediaType: value.mediaType,
    sizeBytes: value.sizeBytes,
    revisionCount: value.revisionCount,
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

function isOpaqueId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value !== "00000000-0000-0000-0000-000000000000" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
      value,
    )
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isExactRecord(
  value: unknown,
  keys: readonly string[],
): value is Record<string, unknown> {
  if (!isRecord(value)) {
    return false;
  }
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}
