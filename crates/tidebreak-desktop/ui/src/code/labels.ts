import type {
  CodePermissionMode,
  CodeSessionLifecycle,
  CodeWorkspaceStatus,
  FenceReason,
  HarnessKind,
  HarnessTier,
} from "../api/types";

/**
 * Display names for the code-mode vocabulary.
 *
 * The wire tokens are engine and lifecycle identifiers. These strings are
 * what the rail, the doctor, and the workspace header actually show.
 */

export const HARNESS_LABELS: Record<HarnessKind, string> = {
  claude_code: "Claude Code",
  codex: "Codex CLI",
  opencode: "opencode",
  grok: "Grok CLI",
};

export const HARNESS_TIER_LABELS: Record<HarnessTier, string> = {
  reference: "Reference",
  secondary: "Secondary",
  tertiary: "Tertiary",
  best_effort: "Best effort",
};

export const LIFECYCLE_LABELS: Record<CodeSessionLifecycle, string> = {
  created: "Created",
  idle: "Idle",
  running: "Running",
  fenced: "Fenced",
  ended: "Ended",
};

export const WORKSPACE_STATUS_LABELS: Record<CodeWorkspaceStatus, string> = {
  creating: "Creating",
  setup_failed: "Setup failed",
  active: "Active",
  archived: "Archived",
};

export const PERMISSION_MODE_LABELS: Record<CodePermissionMode, string> = {
  plan: "Plan",
  ask: "Ask",
  auto: "Auto",
};

/** The server refuses Ask and Auto until a later phase. */
export const PERMISSION_MODE_UNAVAILABLE_REASON =
  "not yet available; create the session in plan mode";

export function fenceReasonText(reason: FenceReason): string {
  if (reason.type === "orphan_alive") {
    return "An engine process is still running from a previous session. Reap it before starting another turn.";
  }
  return reason.detail;
}

export function isHarnessReady(entry: {
  found: boolean;
  authenticated?: boolean;
}): boolean {
  return entry.found && entry.authenticated !== false;
}
