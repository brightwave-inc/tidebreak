import { invoke } from "@tauri-apps/api/core";

import { hasLocalHostAuthority } from "@/host";

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

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
