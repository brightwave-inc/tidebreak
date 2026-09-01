import { invoke } from "@tauri-apps/api/core";

import type { CitationLocator } from "@/api";
import { isRecord } from "@/lib/guards";

/**
 * Conversation outputs are read and edited over the server's HTTP routes, the
 * same ones a headless client uses — only the native save dialog still goes
 * through the shell (see `exportDeliverable` below).
 *
 * The connection is module state because these functions are called from
 * components, hooks, and cell renderers that have no client in hand; the Tauri
 * bridge they replaced was a process-wide handle too. The shell binds it once,
 * beside the `ApiClient` it builds from the same address and token.
 */
let connection: { baseUrl: string; token: string } | null = null;

export function connectOutputs(baseUrl: string, token: string): void {
  connection = { baseUrl, token };
}

async function outputRequest(
  path: string,
  init?: { method?: string; body?: unknown },
): Promise<Response> {
  if (!connection) throw new Error("Tidebreak is still starting");
  const headers: Record<string, string> = {
    Authorization: `Bearer ${connection.token}`,
  };
  if (init?.body !== undefined) headers["Content-Type"] = "application/json";
  const response = await fetch(`${connection.baseUrl}${path}`, {
    method: init?.method,
    headers,
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
  });
  if (response.ok) return response;
  // Surface the server's own message: these strings are what the panels show.
  let detail = response.statusText;
  try {
    const body = (await response.json()) as { message?: string };
    if (body.message) detail = body.message;
  } catch {
    /* keep the status text */
  }
  throw new Error(detail);
}

async function outputJson<T>(
  path: string,
  init?: { method?: string; body?: unknown },
): Promise<T> {
  return (await (await outputRequest(path, init)).json()) as T;
}

function outputPath(chatId: string, outputId: string): string {
  return `/chats/${encodeURIComponent(chatId)}/outputs/${encodeURIComponent(outputId)}`;
}

export type DeliverableSummary = {
  outputId: string;
  filename: string;
  mediaType: string;
  sizeBytes: number;
  revisionCount: number;
  updatedAt: string;
  // The background run that produced the current revision, when this output was
  // submitted by a background agent rather than written by a foreground
  // turn. `null` for a foreground deliverable.
  producingRunId: string | null;
};

export type DeliverablesCatalog = {
  deliverables: DeliverableSummary[];
  truncated: boolean;
};

export type DeliverablePreview = {
  outputId: string;
  filename: string;
  mediaType: string;
  revisionCount: number;
  /** Revision this preview was built from — keys the inline file viewer. */
  revisionId: string;
  content: string;
  truncated: boolean;
};

/** One revision's complete bytes, for inline viewers (office, PDF, image). */
export type DeliverableFile = {
  outputId: string;
  revisionId: string;
  mediaType: string;
  bytes: Uint8Array;
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

/** The curated model-authored text types (bounded text preview + size ceiling). */
export type TextDeliverableMediaType =
  | "text/markdown"
  | "text/plain"
  | "text/csv"
  | "application/json"
  | "text/html"
  | "application/vnd.tidebreak.chart+json";

const MAX_DELIVERABLES = 100;
const MAX_FILENAME_CHARACTERS = 120;
const MAX_MEDIA_TYPE_CHARACTERS = 127;
const MAX_PREVIEW_CHARACTERS = 100_000;
const MAX_DELIVERABLE_BYTES = 512 * 1024;
const MAX_BINARY_DELIVERABLE_BYTES = 16 * 1024 * 1024;
const MAX_OUTPUT_REVISIONS = 100;
const MAX_OUTPUT_REVISION_SOURCES = 20;
const MAX_SOURCE_LABEL_CHARACTERS = 512;
const MAX_SOURCE_URL_CHARACTERS = 2 * 1024;

export function isTextDeliverableMediaType(
  value: string,
): value is TextDeliverableMediaType {
  return [
    "text/markdown",
    "text/plain",
    "text/csv",
    "application/json",
    "text/html",
    "application/vnd.tidebreak.chart+json",
  ].includes(value);
}

/**
 * Whether an output's content can be edited in place, mirroring the native
 * `media_type_is_editable_text`.
 *
 * A plain text box is a faithful editor for Markdown and plain text, including
 * source-code filenames classified as plain text. It is a hazard for the
 * structured document types, where a free-hand edit is as likely to break the
 * document as to fix it.
 */
export function isEditableTextMediaType(value: string): boolean {
  return value === "text/markdown" || value === "text/plain";
}

/** The outcome of saving an edit. A conflict is a state, not a failure. */
export type SaveOutputRevisionResult =
  | { status: "saved"; preview: DeliverablePreview }
  | { status: "conflict"; currentRevisionId: string };

/**
 * Publish an edit of a text output as a new user-authored revision.
 *
 * `expectedRevisionId` is the revision the editor was opened on and is enforced
 * by the server: if another revision became current, nothing is written and the
 * result names the revision to reload.
 */
export async function saveOutputRevision(
  chatId: string,
  outputId: string,
  expectedRevisionId: string,
  content: string,
): Promise<SaveOutputRevisionResult> {
  return parseSaveOutputRevisionResult(
    await outputJson(`${outputPath(chatId, outputId)}/revisions`, {
      method: "POST",
      body: { expectedRevisionId, content },
    }),
  );
}

export function parseSaveOutputRevisionResult(
  value: unknown,
): SaveOutputRevisionResult {
  if (isRecord(value) && value.status === "conflict") {
    if (
      !isExactRecord(value, ["status", "currentRevisionId"]) ||
      !isOpaqueId(value.currentRevisionId)
    ) {
      throw new Error("Invalid output save response");
    }
    return { status: "conflict", currentRevisionId: value.currentRevisionId };
  }
  if (!isRecord(value) || value.status !== "saved") {
    throw new Error("Invalid output save response");
  }
  const { status: _status, ...preview } = value;
  return { status: "saved", preview: parseDeliverablePreview(preview) };
}

export async function listDeliverables(
  chatId: string,
): Promise<DeliverablesCatalog> {
  return parseDeliverablesCatalog(
    await outputJson(`/chats/${encodeURIComponent(chatId)}/outputs`),
  );
}

export async function readDeliverable(
  chatId: string,
  outputId: string,
): Promise<DeliverablePreview> {
  return parseDeliverablePreview(
    await outputJson(outputPath(chatId, outputId)),
  );
}

/**
 * Load one revision's complete bytes for an inline viewer.
 *
 * Omit `revisionId` to read the current revision. Unlike
 * {@link readDeliverable}, this is not a capped text preview and works for
 * binary artifacts the document engines can draw.
 */
export async function readDeliverableFile(
  chatId: string,
  outputId: string,
  revisionId?: string,
): Promise<DeliverableFile> {
  const query =
    revisionId === undefined
      ? ""
      : `?revision_id=${encodeURIComponent(revisionId)}`;
  const response = await outputRequest(
    `${outputPath(chatId, outputId)}/content${query}`,
  );
  return parseDeliverableFile({
    outputId,
    revisionId: response.headers.get("x-tidebreak-revision-id") ?? revisionId,
    mediaType: (response.headers.get("content-type") ?? "")
      .split(";")[0]
      .trim(),
    bytes: new Uint8Array(await response.arrayBuffer()),
  });
}

export async function restoreOutput(
  chatId: string,
  outputId: string,
): Promise<DeliverableSummary> {
  return parseDeliverableSummary(
    await outputJson(`${outputPath(chatId, outputId)}/restore`, {
      method: "POST",
    }),
  );
}

export type OutputRevisionInfo = {
  revisionId: string;
  ordinal: number;
  sizeBytes: number;
  createdAt: string;
  producedBy: "agent" | "backgroundAgent" | "user";
  isCurrent: boolean;
  sources: OutputRevisionSource[];
};

export type OutputRevisionSource =
  | {
      kind: "document";
      citationId: string;
      documentId: string;
      locator: CitationLocator;
    }
  | {
      kind: "web";
      url: string;
      label: string;
      domain: string;
    };

export type OutputRevisionsCatalog = {
  outputId: string;
  revisions: OutputRevisionInfo[];
};

export async function listOutputRevisions(
  chatId: string,
  outputId: string,
): Promise<OutputRevisionsCatalog> {
  return parseOutputRevisionsCatalog(
    await outputJson(`${outputPath(chatId, outputId)}/revisions`),
    outputId,
  );
}

export async function readOutputRevision(
  chatId: string,
  outputId: string,
  revisionId: string,
): Promise<DeliverablePreview> {
  return parseDeliverablePreview(
    await outputJson(
      `${outputPath(chatId, outputId)}/revisions/${encodeURIComponent(revisionId)}`,
    ),
  );
}

export async function restoreOutputRevision(
  chatId: string,
  outputId: string,
  revisionId: string,
): Promise<DeliverableSummary> {
  return parseDeliverableSummary(
    await outputJson(
      `${outputPath(chatId, outputId)}/revisions/${encodeURIComponent(revisionId)}/restore`,
      { method: "POST" },
    ),
  );
}

export async function deleteOutput(
  chatId: string,
  outputId: string,
): Promise<DeliverableSummary> {
  return parseDeliverableSummary(
    await outputJson(outputPath(chatId, outputId), { method: "DELETE" }),
  );
}

export function parseOutputRevisionsCatalog(
  value: unknown,
  expectedOutputId?: string,
): OutputRevisionsCatalog {
  if (
    !isExactRecord(value, ["outputId", "revisions"]) ||
    !isOpaqueId(value.outputId) ||
    (expectedOutputId !== undefined && value.outputId !== expectedOutputId) ||
    !Array.isArray(value.revisions) ||
    value.revisions.length < 1 ||
    value.revisions.length > MAX_OUTPUT_REVISIONS
  ) {
    throw new Error("Invalid output versions response");
  }
  const revisions = value.revisions.map((row): OutputRevisionInfo => {
    if (
      !isExactRecord(row, [
        "revisionId",
        "ordinal",
        "sizeBytes",
        "createdAt",
        "producedBy",
        "isCurrent",
        "sources",
      ]) ||
      !isOpaqueId(row.revisionId) ||
      typeof row.ordinal !== "number" ||
      !Number.isSafeInteger(row.ordinal) ||
      row.ordinal < 1 ||
      row.ordinal > MAX_OUTPUT_REVISIONS ||
      typeof row.sizeBytes !== "number" ||
      !Number.isSafeInteger(row.sizeBytes) ||
      row.sizeBytes < 0 ||
      typeof row.createdAt !== "string" ||
      !Number.isFinite(Date.parse(row.createdAt)) ||
      (row.producedBy !== "agent" &&
        row.producedBy !== "backgroundAgent" &&
        row.producedBy !== "user") ||
      typeof row.isCurrent !== "boolean" ||
      !Array.isArray(row.sources) ||
      row.sources.length > MAX_OUTPUT_REVISION_SOURCES
    ) {
      throw new Error("Invalid output versions response");
    }
    const sources = row.sources.map(parseOutputRevisionSource);
    return {
      revisionId: row.revisionId,
      ordinal: row.ordinal,
      sizeBytes: row.sizeBytes,
      createdAt: row.createdAt,
      producedBy: row.producedBy,
      isCurrent: row.isCurrent,
      sources,
    };
  });
  return { outputId: value.outputId, revisions };
}

function parseOutputRevisionSource(value: unknown): OutputRevisionSource {
  if (!isRecord(value)) throw new Error("Invalid output versions response");
  if (value.kind === "document") {
    if (
      !isExactRecord(value, ["kind", "citationId", "documentId", "locator"]) ||
      !isOpaqueId(value.citationId) ||
      !isOpaqueId(value.documentId) ||
      !isCitationLocator(value.locator)
    ) {
      throw new Error("Invalid output versions response");
    }
    return {
      kind: "document",
      citationId: value.citationId,
      documentId: value.documentId,
      locator: value.locator,
    };
  }
  if (
    value.kind !== "web" ||
    !isExactRecord(value, ["kind", "url", "label", "domain"]) ||
    !isWebSourceUrl(value.url) ||
    !isBoundedSourceText(value.label) ||
    !isBoundedSourceText(value.domain)
  ) {
    throw new Error("Invalid output versions response");
  }
  return {
    kind: "web",
    url: value.url,
    label: value.label,
    domain: value.domain,
  };
}

function isCitationLocator(value: unknown): value is CitationLocator {
  if (!isRecord(value) || typeof value.kind !== "string") return false;
  const positiveInteger = (candidate: unknown): candidate is number =>
    typeof candidate === "number" &&
    Number.isSafeInteger(candidate) &&
    candidate >= 1 &&
    candidate <= 10_000_000;
  switch (value.kind) {
    case "document":
      return isExactRecord(value, ["kind"]);
    case "page":
      return (
        isExactRecord(value, ["kind", "page"]) && positiveInteger(value.page)
      );
    case "pages":
    case "lines":
      return (
        isExactRecord(value, ["kind", "start", "end"]) &&
        positiveInteger(value.start) &&
        positiveInteger(value.end) &&
        value.start <= value.end
      );
    case "sheet":
      return (
        isExactRecord(value, ["kind", "sheet", "cells"]) &&
        typeof value.sheet === "string" &&
        value.sheet.length > 0 &&
        value.sheet.length <= 120 &&
        !/\p{Cc}/u.test(value.sheet) &&
        (value.cells === null ||
          (typeof value.cells === "string" &&
            value.cells.length > 0 &&
            value.cells.length <= 32))
      );
    default:
      return false;
  }
}

function isWebSourceUrl(value: unknown): value is string {
  if (typeof value !== "string" || value.length > MAX_SOURCE_URL_CHARACTERS) {
    return false;
  }
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function isBoundedSourceText(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= MAX_SOURCE_LABEL_CHARACTERS &&
    !/\p{Cc}/u.test(value)
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
      "revisionCount",
      "revisionId",
      "content",
      "truncated",
    ]) ||
    !isOpaqueId(value.outputId) ||
    !isDeliverableFilename(value.filename) ||
    !isDeliverableMediaType(value.mediaType) ||
    typeof value.revisionCount !== "number" ||
    !Number.isSafeInteger(value.revisionCount) ||
    value.revisionCount < 1 ||
    value.revisionCount > MAX_OUTPUT_REVISIONS ||
    !isOpaqueId(value.revisionId) ||
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
    revisionCount: value.revisionCount,
    revisionId: value.revisionId,
    content: value.content,
    truncated: value.truncated,
  };
}

export function parseDeliverableFile(value: unknown): DeliverableFile {
  if (
    !isExactRecord(value, ["outputId", "revisionId", "mediaType", "bytes"]) ||
    !isOpaqueId(value.outputId) ||
    !isOpaqueId(value.revisionId) ||
    !isDeliverableMediaType(value.mediaType) ||
    !(value.bytes instanceof Uint8Array)
  ) {
    throw new Error("Invalid output file response");
  }
  if (
    value.bytes.byteLength === 0 ||
    value.bytes.byteLength > deliverableByteCeiling(value.mediaType)
  ) {
    throw new Error("Invalid output file response");
  }
  return {
    outputId: value.outputId,
    revisionId: value.revisionId,
    mediaType: value.mediaType,
    bytes: value.bytes,
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

export function parseDeliverableSummary(value: unknown): DeliverableSummary {
  if (
    !isExactRecord(value, [
      "outputId",
      "filename",
      "mediaType",
      "sizeBytes",
      "revisionCount",
      "updatedAt",
      "producingRunId",
    ]) ||
    !isOpaqueId(value.outputId) ||
    !isDeliverableFilename(value.filename) ||
    !isDeliverableMediaType(value.mediaType) ||
    typeof value.sizeBytes !== "number" ||
    !Number.isSafeInteger(value.sizeBytes) ||
    value.sizeBytes < 0 ||
    value.sizeBytes > deliverableByteCeiling(value.mediaType) ||
    typeof value.revisionCount !== "number" ||
    !Number.isSafeInteger(value.revisionCount) ||
    value.revisionCount < 1 ||
    value.revisionCount > MAX_OUTPUT_REVISIONS ||
    typeof value.updatedAt !== "string" ||
    !Number.isFinite(Date.parse(value.updatedAt)) ||
    (value.producingRunId !== null && !isOpaqueId(value.producingRunId))
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
    producingRunId: value.producingRunId,
  };
}

function isDeliverableFilename(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    [...value].length <= MAX_FILENAME_CHARACTERS &&
    /^[A-Za-z0-9][A-Za-z0-9 _().-]*$/.test(value) &&
    !value.endsWith(".") &&
    value.trim() === value
  );
}

// Mirrors the native `validate_deliverable_media_type`: a bounded, well-formed
// `type/subtype` token with no parameters. Binary artifacts carry arbitrary
// media types, so this is a shape check, not an allowlist.
function isDeliverableMediaType(value: unknown): value is string {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.length > MAX_MEDIA_TYPE_CHARACTERS
  ) {
    return false;
  }
  const parts = value.split("/");
  return (
    parts.length === 2 &&
    parts.every((token) => /^[A-Za-z0-9!#$&^_.+-]+$/.test(token))
  );
}

function deliverableByteCeiling(mediaType: unknown): number {
  return typeof mediaType === "string" && isTextDeliverableMediaType(mediaType)
    ? MAX_DELIVERABLE_BYTES
    : MAX_BINARY_DELIVERABLE_BYTES;
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

function isExactRecord(
  value: unknown,
  keys: readonly string[],
): value is Record<string, unknown> {
  if (!isRecord(value)) {
    return false;
  }
  const actual = Object.keys(value);
  return (
    actual.length === keys.length && actual.every((key) => keys.includes(key))
  );
}
