import {
  isFiniteNumber,
  isMember,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeFileChange,
  CodeWorkspaceDiff,
  CodeWorkspaceFiles,
  CodeWorkspaceSearch,
  CodeWorkspaceBlob,
  CodeWorkspaceTree,
  Diffstat,
} from "../../api/types";
import type {
  CodeWorkspaceDiff as WireCodeWorkspaceDiff,
  CodeWorkspaceFiles as WireCodeWorkspaceFiles,
  CodeWorkspaceHistorySearchMatch as WireCodeWorkspaceHistorySearchMatch,
  CodeWorkspaceSearch as WireCodeWorkspaceSearch,
  CodeWorkspaceSearchMatch as WireCodeWorkspaceSearchMatch,
  CodeWorkspaceBlob as WireCodeWorkspaceBlob,
  CodeWorkspaceFileSaved as WireCodeWorkspaceFileSaved,
  CodeFileChange as WireCodeFileChange,
  CheckpointRestoreTarget as WireCheckpointRestoreTarget,
  CodeCheckpointRestorePreview as WireCodeCheckpointRestorePreview,
  CodeCheckpointRestoreResult as WireCodeCheckpointRestoreResult,
  CodeRestoreAffectedTurn,
  CodeWorktreeChange as WireCodeWorktreeChange,
  Diffstat as WireDiffstat,
} from "../../generated/wire";
import {
  lineText,
  nonEmptyLine,
  optionalLine,
  rawText,
  wireId,
  optionalWireId,
  timestamp,
  HARNESS_KINDS,
  FILE_CHANGE_KINDS,
} from "./shared";

function parseWorkspaceContentRevision(
  value: unknown,
): CodeWorkspaceTree["revision"] | false {
  if (value === undefined) return undefined;
  if (value === "live" || value === "retained") return value;
  return false;
}

type WorkspaceContentSource = Pick<
  CodeWorkspaceTree,
  "revision" | "revision_ref" | "revision_saved_at"
>;

/** Which checkpoint a remote payload was read from, and when it was saved. */
function parseWorkspaceContentSource(
  value: Record<string, unknown>,
): WorkspaceContentSource | null {
  const revision = parseWorkspaceContentRevision(value.revision);
  if (revision === false) return null;
  if (value.revision_ref !== undefined && !nonEmptyLine(value.revision_ref)) {
    return null;
  }
  if (
    value.revision_saved_at !== undefined &&
    !timestamp(value.revision_saved_at)
  ) {
    return null;
  }
  return {
    ...(revision === undefined ? {} : { revision }),
    ...(value.revision_ref === undefined
      ? {}
      : { revision_ref: value.revision_ref }),
    ...(value.revision_saved_at === undefined
      ? {}
      : { revision_saved_at: value.revision_saved_at }),
  };
}

export function parseCodeWorkspaceTree(
  value: unknown,
): CodeWorkspaceTree | null {
  if (
    !isRecord(value) ||
    !onlyKeys<CodeWorkspaceTree>(value, [
      "paths",
      "truncated",
      "revision",
      "revision_ref",
      "revision_saved_at",
    ]) ||
    !Array.isArray(value.paths) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  const paths: string[] = [];
  for (const item of value.paths) {
    if (!nonEmptyLine(item)) return null;
    paths.push(item);
  }
  const source = parseWorkspaceContentSource(value);
  if (!source) return null;
  return {
    paths,
    truncated: value.truncated,
    ...source,
  };
}

export function parseCodeWorkspaceSearch(
  value: unknown,
): CodeWorkspaceSearch | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceSearch>(value, [
      "matches",
      "history_matches",
      "truncated",
    ]) ||
    !Array.isArray(value.matches) ||
    (value.history_matches !== undefined &&
      !Array.isArray(value.history_matches)) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  const matches: WireCodeWorkspaceSearchMatch[] = [];
  for (const item of value.matches) {
    if (
      !isRecord(item) ||
      !onlyKeys<WireCodeWorkspaceSearchMatch>(item, [
        "path",
        "line_number",
        "line",
      ]) ||
      !nonEmptyLine(item.path) ||
      !isFiniteNumber(item.line_number) ||
      item.line_number < 1 ||
      !rawText(item.line)
    ) {
      return null;
    }
    matches.push({
      path: item.path,
      line_number: item.line_number,
      line: item.line,
    });
  }
  const historyMatches: WireCodeWorkspaceHistorySearchMatch[] = [];
  for (const item of value.history_matches ?? []) {
    if (
      !isRecord(item) ||
      !onlyKeys<WireCodeWorkspaceHistorySearchMatch>(item, [
        "workspace_id",
        "workspace_title",
        "session_id",
        "turn_id",
        "source",
        "preview",
        "created_at",
      ]) ||
      !wireId(item.workspace_id) ||
      !nonEmptyLine(item.workspace_title) ||
      !wireId(item.session_id) ||
      (item.turn_id !== undefined && !wireId(item.turn_id)) ||
      !["turn_user_input", "turn_narrative", "event"].includes(
        item.source as string,
      ) ||
      !rawText(item.preview) ||
      !timestamp(item.created_at)
    ) {
      return null;
    }
    historyMatches.push({
      workspace_id: item.workspace_id,
      workspace_title: item.workspace_title,
      session_id: item.session_id,
      ...(item.turn_id === undefined ? {} : { turn_id: item.turn_id }),
      source: item.source as WireCodeWorkspaceHistorySearchMatch["source"],
      preview: item.preview,
      created_at: item.created_at,
    });
  }
  return {
    matches,
    ...(value.history_matches === undefined
      ? {}
      : { history_matches: historyMatches }),
    truncated: value.truncated,
  };
}

export function parseCodeWorkspaceFiles(
  value: unknown,
): CodeWorkspaceFiles | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceFiles>(value, [
      "files",
      "truncated",
      "stat",
      "turn_id",
      "revision",
      "revision_ref",
      "revision_saved_at",
      "worktree_tree",
    ]) ||
    !Array.isArray(value.files) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  const files: CodeFileChange[] = [];
  for (const item of value.files) {
    const parsed = parseCodeFileChange(item);
    if (!parsed) return null;
    files.push(parsed);
  }
  const stat = parseDiffstat(value.stat);
  if (!stat) return null;
  if (value.turn_id !== undefined && !wireId(value.turn_id)) return null;
  if (value.worktree_tree !== undefined && !wireId(value.worktree_tree)) {
    return null;
  }
  const source = parseWorkspaceContentSource(value);
  if (!source) return null;
  return {
    files,
    truncated: value.truncated,
    stat,
    ...(value.turn_id !== undefined ? { turn_id: value.turn_id } : {}),
    ...source,
    ...(value.worktree_tree !== undefined
      ? { worktree_tree: value.worktree_tree }
      : {}),
  };
}

/** A file's SHA-256 as the server writes it: 64 lowercase hex digits. */
const CONTENT_HASH = /^[0-9a-f]{64}$/;

const contentHash = (value: unknown): value is string =>
  typeof value === "string" && CONTENT_HASH.test(value);

export function parseCodeWorkspaceBlob(
  value: unknown,
): CodeWorkspaceBlob | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceBlob>(value, [
      "path",
      "content",
      "truncated",
      "binary",
      "hash",
      "revision",
      "revision_ref",
      "revision_saved_at",
    ]) ||
    !lineText(value.path) ||
    !rawText(value.content) ||
    typeof value.truncated !== "boolean" ||
    typeof value.binary !== "boolean" ||
    (value.hash !== undefined && !contentHash(value.hash))
  ) {
    return null;
  }
  const source = parseWorkspaceContentSource(value);
  if (!source) return null;
  return {
    path: value.path,
    content: value.content,
    truncated: value.truncated,
    binary: value.binary,
    ...(value.hash !== undefined ? { hash: value.hash } : {}),
    ...source,
  };
}

/** `PUT /code/workspaces/{id}/file`: the saved file and its next base. */
export function parseCodeWorkspaceFileSaved(
  value: unknown,
): WireCodeWorkspaceFileSaved | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceFileSaved>(value, ["path", "hash"]) ||
    !lineText(value.path) ||
    !contentHash(value.hash)
  ) {
    return null;
  }
  return { path: value.path, hash: value.hash };
}

export function parseCodeWorkspaceDiff(
  value: unknown,
): CodeWorkspaceDiff | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceDiff>(value, [
      "diff",
      "truncated",
      "stat",
      "turn_id",
      "file",
      "revision",
      "revision_ref",
      "revision_saved_at",
    ]) ||
    !rawText(value.diff) ||
    typeof value.truncated !== "boolean" ||
    !optionalWireId(value.turn_id) ||
    !optionalLine(value.file)
  ) {
    return null;
  }
  const stat = parseDiffstat(value.stat);
  if (!stat) return null;
  const source = parseWorkspaceContentSource(value);
  if (!source) return null;
  return {
    diff: value.diff,
    truncated: value.truncated,
    stat,
    ...(value.turn_id !== undefined ? { turn_id: value.turn_id } : {}),
    ...(value.file !== undefined ? { file: value.file } : {}),
    ...source,
  };
}

function parseCodeFileChange(value: unknown): CodeFileChange | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeFileChange>(value, [
      "path",
      "kind",
      "insertions",
      "deletions",
      "previous_path",
      "uncommitted",
    ]) ||
    !lineText(value.path) ||
    !isMember(value.kind, FILE_CHANGE_KINDS) ||
    !isFiniteNumber(value.insertions) ||
    !isFiniteNumber(value.deletions) ||
    !optionalLine(value.previous_path) ||
    (value.uncommitted !== undefined && typeof value.uncommitted !== "boolean")
  ) {
    return null;
  }
  return {
    path: value.path,
    kind: value.kind,
    insertions: value.insertions,
    deletions: value.deletions,
    ...(value.previous_path !== undefined
      ? { previous_path: value.previous_path }
      : {}),
    ...(value.uncommitted !== undefined
      ? { uncommitted: value.uncommitted }
      : {}),
  };
}

function parseCodeFileChanges(value: unknown): CodeFileChange[] | null {
  if (!Array.isArray(value)) return null;
  const files: CodeFileChange[] = [];
  for (const item of value) {
    const parsed = parseCodeFileChange(item);
    if (!parsed) return null;
    files.push(parsed);
  }
  return files;
}

/** Where a checkpoint restore puts the worktree back to. */
export function parseCheckpointRestoreTarget(
  value: unknown,
): WireCheckpointRestoreTarget | null {
  if (!isRecord(value)) return null;
  if (value.kind === "before_turn") {
    if (
      !onlyKeys<Extract<WireCheckpointRestoreTarget, { kind: "before_turn" }>>(
        value,
        ["kind", "turn_id"],
      ) ||
      !wireId(value.turn_id)
    ) {
      return null;
    }
    return { kind: "before_turn", turn_id: value.turn_id };
  }
  if (value.kind === "before_restore") {
    if (
      !onlyKeys<
        Extract<WireCheckpointRestoreTarget, { kind: "before_restore" }>
      >(value, ["kind", "restore_id"]) ||
      !wireId(value.restore_id)
    ) {
      return null;
    }
    return { kind: "before_restore", restore_id: value.restore_id };
  }
  return null;
}

/** What a checkpoint restore would undo, read before anything moves. */
export function parseCodeCheckpointRestorePreview(
  value: unknown,
): WireCodeCheckpointRestorePreview | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCheckpointRestorePreview>(value, [
      "target",
      "session_id",
      "files",
      "truncated",
      "stat",
      "current_tree",
      "blocked",
      "affected_turns",
    ]) ||
    !wireId(value.session_id) ||
    typeof value.truncated !== "boolean" ||
    !wireId(value.current_tree) ||
    !Array.isArray(value.blocked) ||
    !value.blocked.every(wireId) ||
    !Array.isArray(value.affected_turns)
  ) {
    return null;
  }
  const target = parseCheckpointRestoreTarget(value.target);
  const files = parseCodeFileChanges(value.files);
  const stat = parseDiffstat(value.stat);
  if (!target || !files || !stat) return null;
  const affectedTurns: CodeRestoreAffectedTurn[] = [];
  for (const turn of value.affected_turns) {
    if (
      !isRecord(turn) ||
      !onlyKeys<CodeRestoreAffectedTurn>(turn, [
        "session_id",
        "turn_id",
        "ordinal",
        "harness_kind",
      ]) ||
      !wireId(turn.session_id) ||
      !wireId(turn.turn_id) ||
      !isFiniteNumber(turn.ordinal) ||
      !isMember(turn.harness_kind, HARNESS_KINDS)
    ) {
      return null;
    }
    affectedTurns.push({
      session_id: turn.session_id,
      turn_id: turn.turn_id,
      ordinal: turn.ordinal,
      harness_kind: turn.harness_kind,
    });
  }
  return {
    target,
    session_id: value.session_id,
    files,
    truncated: value.truncated,
    stat,
    current_tree: value.current_tree,
    blocked: value.blocked,
    affected_turns: affectedTurns,
  };
}

/** A checkpoint restore that landed. */
export function parseCodeCheckpointRestoreResult(
  value: unknown,
): WireCodeCheckpointRestoreResult | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCheckpointRestoreResult>(value, [
      "restore_id",
      "target",
      "session_id",
      "files",
      "truncated",
      "stat",
    ]) ||
    !wireId(value.restore_id) ||
    !wireId(value.session_id) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  const target = parseCheckpointRestoreTarget(value.target);
  const files = parseCodeFileChanges(value.files);
  const stat = parseDiffstat(value.stat);
  if (!target || !files || !stat) return null;
  return {
    restore_id: value.restore_id,
    target,
    session_id: value.session_id,
    files,
    truncated: value.truncated,
    stat,
  };
}

/** The files a revert or a discard changed. */
export function parseCodeWorktreeChange(
  value: unknown,
): WireCodeWorktreeChange | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorktreeChange>(value, ["paths"]) ||
    !Array.isArray(value.paths) ||
    !value.paths.every((path) => lineText(path))
  ) {
    return null;
  }
  return { paths: value.paths as string[] };
}

export function parseDiffstat(value: unknown): Diffstat | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireDiffstat>(value, [
      "files",
      "insertions",
      "deletions",
      "truncated",
    ]) ||
    !isFiniteNumber(value.files) ||
    !isFiniteNumber(value.insertions) ||
    !isFiniteNumber(value.deletions) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  return {
    files: value.files,
    insertions: value.insertions,
    deletions: value.deletions,
    truncated: value.truncated,
  };
}
