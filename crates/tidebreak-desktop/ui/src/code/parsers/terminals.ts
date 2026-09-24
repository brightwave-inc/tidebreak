import {
  isFiniteNumber,
  isMember,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeTerminalRead,
  CodeTerminalSnapshot,
  HarnessSignInRead,
  HarnessSignInTerminal,
} from "../../api/types";
import type {
  CodeTerminalRead as WireCodeTerminalRead,
  CodeTerminalSnapshot as WireCodeTerminalSnapshot,
  HarnessSignInRead as WireHarnessSignInRead,
  HarnessSignInTerminal as WireHarnessSignInTerminal,
} from "../../generated/wire";
import { lineText, rawText, wireId, timestamp, HARNESS_KINDS } from "./shared";

export function parseCodeTerminal(value: unknown): CodeTerminalSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeTerminalSnapshot>(value, [
      "id",
      "workspace_id",
      "cols",
      "rows",
      "ended",
      "created_at",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.workspace_id) ||
    !isFiniteNumber(value.cols) ||
    !isFiniteNumber(value.rows) ||
    typeof value.ended !== "boolean" ||
    !timestamp(value.created_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    workspace_id: value.workspace_id,
    cols: value.cols,
    rows: value.rows,
    ended: value.ended,
    created_at: value.created_at,
  };
}

export function parseCodeTerminalList(
  value: unknown,
): CodeTerminalSnapshot[] | null {
  if (!Array.isArray(value)) return null;
  const terminals: CodeTerminalSnapshot[] = [];
  for (const item of value) {
    const parsed = parseCodeTerminal(item);
    if (!parsed) return null;
    terminals.push(parsed);
  }
  return terminals;
}

export function parseHarnessSignInTerminal(
  value: unknown,
): HarnessSignInTerminal | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessSignInTerminal>(value, [
      "id",
      "kind",
      "command",
      "cols",
      "rows",
      "ended",
      "created_at",
    ]) ||
    !wireId(value.id) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    !lineText(value.command) ||
    !isFiniteNumber(value.cols) ||
    !isFiniteNumber(value.rows) ||
    typeof value.ended !== "boolean" ||
    !timestamp(value.created_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    kind: value.kind,
    command: value.command,
    cols: value.cols,
    rows: value.rows,
    ended: value.ended,
    created_at: value.created_at,
  };
}

export function parseHarnessSignInRead(
  value: unknown,
): HarnessSignInRead | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessSignInRead>(value, [
      "id",
      "kind",
      "bytes",
      "cursor",
      "overflow",
      "truncated",
      "ended",
    ]) ||
    !wireId(value.id) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    !rawText(value.bytes) ||
    !isFiniteNumber(value.cursor) ||
    typeof value.overflow !== "boolean" ||
    typeof value.truncated !== "boolean" ||
    typeof value.ended !== "boolean"
  ) {
    return null;
  }
  return {
    id: value.id,
    kind: value.kind,
    bytes: value.bytes,
    cursor: value.cursor,
    overflow: value.overflow,
    truncated: value.truncated,
    ended: value.ended,
  };
}

export function parseCodeTerminalRead(value: unknown): CodeTerminalRead | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeTerminalRead>(value, [
      "id",
      "workspace_id",
      "bytes",
      "cursor",
      "overflow",
      "truncated",
      "ended",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.workspace_id) ||
    !rawText(value.bytes) ||
    !isFiniteNumber(value.cursor) ||
    typeof value.overflow !== "boolean" ||
    typeof value.truncated !== "boolean" ||
    typeof value.ended !== "boolean"
  ) {
    return null;
  }
  return {
    id: value.id,
    workspace_id: value.workspace_id,
    bytes: value.bytes,
    cursor: value.cursor,
    overflow: value.overflow,
    truncated: value.truncated,
    ended: value.ended,
  };
}
