import type {
  Attention,
  CodePermissionMode,
  CodeSessionLifecycle,
  CodeWorkspaceStatus,
  FenceReason,
  HarnessCaps,
  HarnessKind,
  HarnessTier,
  ModelInfo,
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

/** One-line product gloss for the picker. Not how the adapter works. */
export const HARNESS_SUBTITLES: Record<HarnessKind, string> = {
  claude_code: "Anthropic's coding agent",
  codex: "OpenAI's coding agent",
  opencode: "Open-source coding agent",
  grok: "xAI's coding agent",
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
  allow: "Allow all",
};

/** Shown when the selected harness cannot honor Ask or Auto. */
export const PERMISSION_MODE_UNAVAILABLE_REASON =
  "this harness cannot honor that mode";

/** Stated wherever unsupervised Auto is offered (decision 0038). */
export const UNSUPERVISED_AUTO_NOTE =
  "This engine has no approval channel: in Auto, every action runs without asking.";

/** Stated wherever Allow is offered (decision 0039). */
export const ALLOW_ALL_NOTE =
  "This engine's permission system is off: every action runs without asking.";

/** The capability flags the mode policy reads. */
export type ModeCaps = Pick<
  HarnessCaps,
  "plan_mode" | "structured_approvals" | "auto_mode" | "allow_mode"
>;

export function fenceReasonText(reason: FenceReason): string {
  if (reason.type === "orphan_alive") {
    return "An engine process is still running from a previous session. Reap it before starting another turn.";
  }
  if (reason.type === "resume_lost") {
    return `The engine no longer has this session, so it cannot continue (${reason.detail}). Reap it to start a fresh engine session in this workspace; the transcript above is kept.`;
  }
  return reason.detail;
}

export function isHarnessReady(entry: {
  found: boolean;
  authenticated?: boolean;
}): boolean {
  return entry.found && entry.authenticated !== false;
}

/** True when create can post at least one permission mode this engine honors. */
export function harnessHonorsAnyCreateMode(entry: { caps: ModeCaps }): boolean {
  return createPermissionModes(entry.caps).length > 0;
}

/**
 * Why a picker row is not selectable. Ready rows return null.
 * Versions, paths, and capability names stay on the doctor.
 */
export function harnessUnusableReason(entry: {
  found: boolean;
  authenticated?: boolean;
  caps: ModeCaps;
}): string | null {
  if (!entry.found) return "Not installed";
  if (entry.authenticated === false) return "Sign in via your terminal";
  if (!harnessHonorsAnyCreateMode(entry)) return "Not available yet";
  return null;
}

/**
 * True when this engine's Auto runs with nobody to ask (decision 0038):
 * it has an auto posture but no approval channel to escalate through.
 */
export function autoIsUnsupervised(caps: ModeCaps): boolean {
  return caps.auto_mode === "supported" && caps.structured_approvals !== "supported";
}

/**
 * Create default: Allow all when the engine can honor it. Otherwise the
 * most autonomous posture it actually has.
 */
export function defaultCreatePermissionMode(caps: ModeCaps): CodePermissionMode {
  if (caps.allow_mode === "supported") return "allow";
  if (caps.auto_mode === "supported") return "auto";
  if (caps.structured_approvals === "supported") return "ask";
  if (caps.plan_mode === "supported") return "plan";
  return "allow";
}

/** The modes create may post for this engine, each on its own flag. */
export function createPermissionModes(caps: ModeCaps): CodePermissionMode[] {
  const modes: CodePermissionMode[] = [];
  if (caps.plan_mode === "supported") modes.push("plan");
  if (caps.structured_approvals === "supported") modes.push("ask");
  if (caps.auto_mode === "supported") modes.push("auto");
  if (caps.allow_mode === "supported") modes.push("allow");
  return modes;
}

export type CodeModelOption = {
  id: string;
  label: string;
  source: string;
  default?: boolean;
};

/** Gateway models the picker may offer when this profile is on model-gateway. */
export function gatewayCodeModels(
  models: readonly ModelInfo[],
  kind: HarnessKind,
  defaultKey?: string | null,
): CodeModelOption[] {
  const source = `${HARNESS_LABELS[kind]} · model-gateway`;
  return models
    .filter((model) => model.provider === "model_gateway" && model.available)
    .map((model) => ({
      id: model.id,
      label: model.display_name,
      source,
      default: defaultKey === model.key || defaultKey === model.id,
    }));
}

export function harnessCodeModels(
  listed: readonly { id: string; label: string; default?: boolean }[],
  kind: HarnessKind,
): CodeModelOption[] {
  const source = HARNESS_LABELS[kind];
  return listed.map((option) => ({
    id: option.id,
    label: prettyCodeModelLabel(option.label || option.id),
    source,
    default: option.default,
  }));
}

export function prettyCodeModelLabel(id: string): string {
  const leaf = id.split("/").pop() ?? id;
  if (leaf.includes(" ") && /[A-Z]/.test(leaf)) return leaf;
  return leaf
    .split("-")
    .filter(Boolean)
    .map((part) => (part.toLowerCase() === "gpt" ? "GPT" : part[0]?.toUpperCase() + part.slice(1)))
    .join(" ");
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
