import { invoke } from "@tauri-apps/api/core";

import { hasLocalHostAuthority } from "@/host";
import { isRecord } from "@/lib/guards";
import {
  currentEditorPreference,
  type ExternalEditorId,
} from "./editorPreference";

export type CodeWorktreeOpenReason =
  | "code_worktree_authority_unavailable"
  | "code_worktree_workspace_unavailable"
  | "code_worktree_workspace_inactive"
  | "code_worktree_path_invalid"
  | "code_worktree_path_not_found"
  | "code_worktree_not_directory"
  | "code_worktree_launcher_unavailable"
  | "code_worktree_open_failed";

const OPEN_REASONS = new Set<CodeWorktreeOpenReason>([
  "code_worktree_authority_unavailable",
  "code_worktree_workspace_unavailable",
  "code_worktree_workspace_inactive",
  "code_worktree_path_invalid",
  "code_worktree_path_not_found",
  "code_worktree_not_directory",
  "code_worktree_launcher_unavailable",
  "code_worktree_open_failed",
]);

export class CodeWorktreeOpenError extends Error {
  readonly reason: CodeWorktreeOpenReason;
  readonly detail: string | null;

  constructor(reason: CodeWorktreeOpenReason, detail: string | null = null) {
    super(reason);
    this.name = "CodeWorktreeOpenError";
    this.reason = reason;
    this.detail = detail;
  }
}

export type CodeWorktreePlatform =
  | "macos"
  | "windows"
  | "linux"
  | "unsupported";

export function codeWorktreePlatform(userAgent: string): CodeWorktreePlatform {
  if (userAgent.includes("Mac OS")) return "macos";
  if (userAgent.includes("Windows")) return "windows";
  if (userAgent.includes("Linux")) return "linux";
  return "unsupported";
}

/** Whether this window can open paths that belong to the active Code server. */
export function canOpenLocalCodeWorktree(
  userAgent = globalThis.navigator?.userAgent ?? "",
): boolean {
  return (
    hasLocalHostAuthority() && codeWorktreePlatform(userAgent) !== "unsupported"
  );
}

/** Ask the native shell to open one local worktree in the system file manager. */
export async function openCodeWorktree(workspaceId: string): Promise<void> {
  try {
    const result: unknown = await invoke("open_code_worktree", { workspaceId });
    if (!isExactOpenedResult(result)) {
      throw new CodeWorktreeOpenError("code_worktree_open_failed");
    }
  } catch (error) {
    if (error instanceof CodeWorktreeOpenError) throw error;
    throw parseCodeWorktreeOpenError(error);
  }
}

export function codeWorktreeOpenFailureMessage(error: unknown): string {
  switch (parseCodeWorktreeOpenError(error).reason) {
    case "code_worktree_authority_unavailable":
      return "This worktree is on the attached machine. Copy the path and open it there.";
    case "code_worktree_workspace_unavailable":
      return "Tidebreak could not verify this local workspace. Refresh it, then try again.";
    case "code_worktree_workspace_inactive":
      return "This workspace no longer has a local worktree. Restore it, or copy the saved path.";
    case "code_worktree_path_invalid":
      return "The saved worktree path is invalid. Copy the path and check the workspace.";
    case "code_worktree_path_not_found":
      return "The worktree folder no longer exists. Refresh or restore the workspace, then try again.";
    case "code_worktree_not_directory":
      return "The worktree path no longer points to a folder. Copy the path and check the workspace.";
    case "code_worktree_launcher_unavailable":
      return "This computer has no file manager command Tidebreak can use. Copy the path and open it manually.";
    case "code_worktree_open_failed":
      return "The file manager could not open this worktree. Copy the path and try again.";
  }
}

export function parseCodeWorktreeOpenError(
  error: unknown,
): CodeWorktreeOpenError {
  if (error instanceof CodeWorktreeOpenError) return error;
  if (
    isRecord(error) &&
    OPEN_REASONS.has(error.reason as CodeWorktreeOpenReason)
  ) {
    return new CodeWorktreeOpenError(
      error.reason as CodeWorktreeOpenReason,
      typeof error.detail === "string" ? error.detail : null,
    );
  }
  return new CodeWorktreeOpenError("code_worktree_open_failed");
}

function isExactOpenedResult(value: unknown): boolean {
  return (
    isRecord(value) &&
    Object.keys(value).length === 1 &&
    value.status === "opened"
  );
}

/* -------------------------------------------------------------------------
 * Open in editor
 *
 * The same shape as the folder open above, one step narrower: the renderer
 * names a workspace and a path relative to it, never a path to run, and the
 * native side re-reads the worktree root and refuses anything outside it.
 * ---------------------------------------------------------------------- */

export type CodeEditorOpenReason =
  | "code_editor_authority_unavailable"
  | "code_editor_workspace_unavailable"
  | "code_editor_workspace_inactive"
  | "code_editor_path_invalid"
  | "code_editor_path_outside_worktree"
  | "code_editor_path_not_found"
  | "code_editor_editor_unknown"
  | "code_editor_editor_unavailable"
  | "code_editor_open_failed";

const EDITOR_REASONS = new Set<CodeEditorOpenReason>([
  "code_editor_authority_unavailable",
  "code_editor_workspace_unavailable",
  "code_editor_workspace_inactive",
  "code_editor_path_invalid",
  "code_editor_path_outside_worktree",
  "code_editor_path_not_found",
  "code_editor_editor_unknown",
  "code_editor_editor_unavailable",
  "code_editor_open_failed",
]);

export class CodeEditorOpenError extends Error {
  readonly reason: CodeEditorOpenReason;
  readonly detail: string | null;

  constructor(reason: CodeEditorOpenReason, detail: string | null = null) {
    super(reason);
    this.name = "CodeEditorOpenError";
    this.reason = reason;
    this.detail = detail;
  }
}

/** One editor's availability on this computer, as the native probe found it. */
export type ExternalEditorProbe = {
  id: ExternalEditorId;
  /** The launcher the probe found, or `null` when the editor is not installed. */
  program: string | null;
};

/**
 * Whether this window can start an editor on the machine holding the worktree.
 *
 * The same test the folder action uses: a window attached to another machine is
 * looking at files it cannot reach, and the editor here would open nothing.
 */
export function canOpenInExternalEditor(
  userAgent = globalThis.navigator?.userAgent ?? "",
): boolean {
  return canOpenLocalCodeWorktree(userAgent);
}

/** Ask the native shell to open one worktree file in the reader's editor. */
export async function openInEditor(input: {
  workspaceId: string;
  /** Relative to the worktree root. Omitted opens the worktree itself. */
  relativePath?: string;
  line?: number;
}): Promise<void> {
  const preference = currentEditorPreference();
  try {
    const result: unknown = await invoke("open_in_editor", {
      request: {
        workspaceId: input.workspaceId,
        relativePath: input.relativePath ?? null,
        line: input.line ?? null,
        editor: preference.editor,
        customProgram:
          preference.editor === "custom" ? preference.customProgram : null,
      },
    });
    if (!isExactOpenedResult(result)) {
      throw new CodeEditorOpenError("code_editor_open_failed");
    }
  } catch (error) {
    if (error instanceof CodeEditorOpenError) throw error;
    throw parseCodeEditorOpenError(error);
  }
}

/** What this computer has installed, for the settings panel to report. */
export async function detectExternalEditors(): Promise<ExternalEditorProbe[]> {
  const result: unknown = await invoke("detect_external_editors");
  if (!Array.isArray(result)) return [];
  return result.filter(
    (probe): probe is ExternalEditorProbe =>
      isRecord(probe) &&
      typeof probe.id === "string" &&
      (probe.program === null || typeof probe.program === "string"),
  );
}

export function codeEditorOpenFailureMessage(error: unknown): string {
  switch (parseCodeEditorOpenError(error).reason) {
    case "code_editor_authority_unavailable":
      return "This file is on the attached machine. Open it in an editor there.";
    case "code_editor_workspace_unavailable":
      return "Tidebreak could not verify this local workspace. Refresh it, then try again.";
    case "code_editor_workspace_inactive":
      return "This workspace no longer has a local worktree. Restore it, then try again.";
    case "code_editor_path_invalid":
    case "code_editor_path_outside_worktree":
      return "That path does not sit inside this workspace's worktree.";
    case "code_editor_path_not_found":
      return "That file is no longer in the worktree. Refresh the file tree, then try again.";
    case "code_editor_editor_unknown":
      return "Your custom editor needs the full path to its program. Set it in Settings, Coding harnesses.";
    case "code_editor_editor_unavailable":
      return "Tidebreak could not find that editor on this computer. Pick another one in Settings, Coding harnesses.";
    case "code_editor_open_failed":
      return "The editor did not start. Try opening the worktree folder instead.";
  }
}

export function parseCodeEditorOpenError(error: unknown): CodeEditorOpenError {
  if (error instanceof CodeEditorOpenError) return error;
  if (
    isRecord(error) &&
    EDITOR_REASONS.has(error.reason as CodeEditorOpenReason)
  ) {
    return new CodeEditorOpenError(
      error.reason as CodeEditorOpenReason,
      typeof error.detail === "string" ? error.detail : null,
    );
  }
  return new CodeEditorOpenError("code_editor_open_failed");
}
