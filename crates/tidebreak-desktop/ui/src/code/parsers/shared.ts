import {
  MAX_WIRE_CURSOR_CHARS,
  MAX_WIRE_ID_CHARS,
  MAX_WIRE_TIMESTAMP_CHARS,
  bounded,
  boundedBlock,
  boundedRaw,
  boundedStringList,
  nonEmptyBounded,
} from "../../lib/wireDecode";
import type {
  PermissionMode,
  CodeSessionLifecycle,
  CodeSessionKind,
  CodeWatchState,
  FileChangeKind,
  CodeWorkspaceStatus,
  HarnessKind,
  ReasoningEffort,
} from "../../api/types";

/**
 * The string bounds every code-mode parser applies, and the enum sets more
 * than one parser module reads. The barrel does not re-export these.
 */

// ---------------------------------------------------------------------------
// String bounds
// ---------------------------------------------------------------------------
//
// Every string this decoder keeps goes through one of three tiers, chosen by
// how the field is drawn rather than by who wrote it:
//
// - Line: drawn on one line, so a control or bidirectional character could
//   redraw or reorder it. Titles, names, branch and base refs, paths, URLs,
//   logins, statuses, models, versions, remediation headlines. The limit
//   holds a PATH_MAX-sized path or a long URL; GitHub caps titles at 256 and
//   refs well under that.
// - Block: drawn as a block, so line breaks, carriage returns, and tabs are
//   structure while everything else a line rejects is still rejected. PR and
//   comment bodies (GitHub caps these at 65,536 characters), commit messages,
//   setup and archive scripts, quick-action commands, model text and
//   reasoning, remediation and error messages, recaps, fence details.
// - Raw: verbatim data the reader asked to see as it is, rendered by a pane
//   that escapes nothing and expects terminal escapes and carriage returns:
//   blob content, diffs and patches, terminal reads, command stdout/stderr,
//   tool result previews, search hit lines and the history excerpts cut from
//   prompts and tool output, the harness's raw approval JSON, and the user's
//   own prompt and steer text. Only the length is bounded,
//   with headroom over the server's own truncation points (512 KiB blobs,
//   256 KiB diffs).
//
// Ids, timestamps, and cursors share the chat decoder's named limits so the
// two clients agree on what a valid payload is. Enum-like discriminators
// (`type`, `kind`) stay presence-only: they are matched, never drawn.

/** Longest one-line field: a PATH_MAX path or a long URL still fits. */
const MAX_CODE_LINE_CHARS = 4_096;

/** Longest authored block: sixteen GitHub-sized bodies, or a long model reply. */
const MAX_CODE_BLOCK_CHARS = 1_048_576;

/** Longest verbatim payload: eight times the server's blob cap. */
const MAX_CODE_RAW_CHARS = 4_194_304;

export const lineText = (value: unknown): value is string =>
  bounded(value, MAX_CODE_LINE_CHARS);
export const nonEmptyLine = (value: unknown): value is string =>
  nonEmptyBounded(value, MAX_CODE_LINE_CHARS);
export const optionalLine = (value: unknown): value is string | undefined =>
  value === undefined || bounded(value, MAX_CODE_LINE_CHARS);
/**
 * A bounded line or `null`. An actor's fields are always present and any of
 * them may be null: the paths that write one know different things about who
 * acted (decision 0086).
 */
export const nullableLine = (value: unknown): value is string | null =>
  value === null || bounded(value, MAX_CODE_LINE_CHARS);
export const lineList = (value: unknown): value is string[] =>
  boundedStringList(value, MAX_CODE_LINE_CHARS);

export const blockText = (value: unknown): value is string =>
  boundedBlock(value, MAX_CODE_BLOCK_CHARS);
export const optionalBlock = (value: unknown): value is string | undefined =>
  value === undefined || boundedBlock(value, MAX_CODE_BLOCK_CHARS);

export const rawText = (value: unknown): value is string =>
  boundedRaw(value, MAX_CODE_RAW_CHARS);
export const optionalRaw = (value: unknown): value is string | undefined =>
  value === undefined || boundedRaw(value, MAX_CODE_RAW_CHARS);

export const wireId = (value: unknown): value is string =>
  nonEmptyBounded(value, MAX_WIRE_ID_CHARS);
export const optionalWireId = (value: unknown): value is string | undefined =>
  value === undefined || nonEmptyBounded(value, MAX_WIRE_ID_CHARS);
/**
 * An id or `null`. A session that binds no workspace (the in-process
 * engine's, decision 0048 step 5) serializes `workspace_id: null` on its
 * snapshot and `workspace: null` on its digest; the key is always present.
 */
export const nullableWireId = (value: unknown): value is string | null =>
  value === null || nonEmptyBounded(value, MAX_WIRE_ID_CHARS);

export const timestamp = (value: unknown): value is string =>
  nonEmptyBounded(value, MAX_WIRE_TIMESTAMP_CHARS);
export const optionalTimestamp = (
  value: unknown,
): value is string | undefined =>
  value === undefined || nonEmptyBounded(value, MAX_WIRE_TIMESTAMP_CHARS);
export const nullableTimestamp = (value: unknown): value is string | null =>
  value === null || nonEmptyBounded(value, MAX_WIRE_TIMESTAMP_CHARS);

export const optionalCursor = (value: unknown): value is string | undefined =>
  value === undefined || nonEmptyBounded(value, MAX_WIRE_CURSOR_CHARS);

// Every kind the server can name on a session, not just the ones the create
// picker offers: the in-process engine reports `internal` (decision 0048
// step 5), and a session it runs must parse like any other.
export const HARNESS_KINDS = new Set<HarnessKind>([
  "claude_code",
  "codex",
  "opencode",
  "grok",
  "internal",
]);

export const PERMISSION_MODES = new Set<PermissionMode>([
  "plan",
  "ask",
  "auto",
  "allow",
]);
export const REASONING_EFFORTS = new Set<ReasoningEffort>([
  "none",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
]);
export const SESSION_LIFECYCLES = new Set<CodeSessionLifecycle>([
  "created",
  "idle",
  "running",
  "fenced",
  "ended",
]);
export const SESSION_KINDS = new Set<CodeSessionKind>(["interactive", "watch"]);

export const WATCH_STATES = new Set<CodeWatchState>([
  "watching",
  "fixing",
  "blocked",
  "done",
  "stopped",
  "failed",
]);

export const WORKSPACE_STATUSES = new Set<CodeWorkspaceStatus>([
  "creating",
  "setup_failed",
  "active",
  "archiving",
  "archived",
  "released",
]);

export const FILE_CHANGE_KINDS = new Set<FileChangeKind>([
  "added",
  "modified",
  "deleted",
  "renamed",
]);
