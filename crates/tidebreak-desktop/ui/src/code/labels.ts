import type {
  Attention,
  CapLevel,
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

/** Shown when the selected harness cannot honor Ask or Auto. */
export const PERMISSION_MODE_UNAVAILABLE_REASON =
  "this harness cannot honor that mode";

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

/** Ask/Auto need a structured approval channel. Plan does not. */
export function harnessHonorsStructuredApprovals(
  structuredApprovals: CapLevel | undefined,
): boolean {
  return structuredApprovals === "supported";
}

/** Create default: Ask when the doctor says the harness can honor it, else Plan. */
export function defaultCreatePermissionMode(
  structuredApprovals: CapLevel | undefined,
): CodePermissionMode {
  return harnessHonorsStructuredApprovals(structuredApprovals) ? "ask" : "plan";
}

export function attentionLabel(attention: Attention): string {
  switch (attention.state.type) {
    case "working":
      return "Working";
    case "needs_you":
      return attention.state.prompt || "Needs you";
    case "stalled":
      return "Stalled";
    case "done_unreviewed":
      return "Done";
    case "fenced":
      return "Fenced";
    case "manual":
      return attention.state.note || "Pinned";
  }
}

export function createPermissionModes(
  structuredApprovals: CapLevel | undefined,
): CodePermissionMode[] {
  return harnessHonorsStructuredApprovals(structuredApprovals)
    ? ["plan", "ask", "auto"]
    : ["plan"];
}
