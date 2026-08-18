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
  ProviderKind,
} from "../api/types";
import { familyForModelId, vendorForModelId } from "../modelFamilies";

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
 * Create default: the most autonomous posture the engine honors, walking
 * Allow → Auto → Ask → Plan (decision 0039, amended 2026-08-18). Approving
 * every step of a fresh session cost more than it caught, so create starts
 * where the work runs. The mode is never silent: whichever surface offers it
 * states the posture next to the control (decisions 0033, 0038).
 */
export function defaultCreatePermissionMode(caps: ModeCaps): CodePermissionMode {
  if (caps.allow_mode === "supported") return "allow";
  if (caps.auto_mode === "supported") return "auto";
  if (caps.structured_approvals === "supported") return "ask";
  return "plan";
}

/** True when this engine honors `--model` (or equivalent) on each turn. */
export function harnessHonorsTurnModel(kind: HarnessKind): boolean {
  return kind === "claude_code" || kind === "grok";
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
  /** The vendor the id is branded as, for icons and grouping. */
  vendor?: ProviderKind | null;
  default?: boolean;
};

/**
 * The vendors a harness can actually drive, or `null` for any. This confines
 * a mixed catalog (a gateway's) to what the engine accepts: Claude Code only
 * takes Claude models, Codex only OpenAI's, Grok only xAI's; opencode is
 * vendor-neutral.
 */
export const HARNESS_VENDORS: Record<
  HarnessKind,
  readonly ProviderKind[] | null
> = {
  claude_code: ["anthropic"],
  codex: ["openai"],
  grok: ["xai"],
  opencode: null,
};

/** The vendor a picker row is branded as: curated match first, then the id. */
export function codeModelVendor(option: {
  id: string;
  vendor?: ProviderKind | null;
}): ProviderKind | null {
  return option.vendor ?? vendorForModelId(option.id);
}

/**
 * Gateway models the picker may offer when this profile is on model-gateway,
 * confined to the vendors the harness can drive.
 */
export function gatewayCodeModels(
  models: readonly ModelInfo[],
  kind: HarnessKind,
  defaultKey?: string | null,
): CodeModelOption[] {
  const source = `${HARNESS_LABELS[kind]} · model-gateway`;
  const allowed = HARNESS_VENDORS[kind];
  return models
    .filter((model) => model.provider === "model_gateway" && model.available)
    .filter((model) => {
      if (!allowed) return true;
      const vendor = model.vendor ?? vendorForModelId(model.id);
      return vendor !== null && allowed.includes(vendor);
    })
    .map((model) => ({
      id: model.id,
      label: model.display_name,
      source,
      vendor: model.vendor ?? vendorForModelId(model.id),
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
    vendor: vendorForModelId(option.id),
    default: option.default,
  }));
}

/** One rail section of the code-mode picker: a vendor and its rows. */
export type CodeModelGroup = {
  id: string;
  label: string;
  iconProvider: ProviderKind;
  iconModelId?: string;
  options: CodeModelOption[];
};

/** Vendors in the rail's fixed order, matching the chat picker's. */
const CODE_VENDOR_ORDER: readonly ProviderKind[] = [
  "openai",
  "anthropic",
  "xai",
  "gemini",
];

const VENDOR_LABELS: Partial<Record<ProviderKind, string>> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
  xai: "xAI",
  gemini: "Google Gemini",
};

/**
 * Group picker rows by vendor the way the chat picker groups a catalog:
 * first-party vendors in a fixed order, then open-model families, then
 * anything unrecognizable under "Other". One group per vendor present.
 */
export function groupCodeModelOptions(
  options: readonly CodeModelOption[],
): CodeModelGroup[] {
  const byId = new Map<string, CodeModelGroup>();
  const add = (
    badge: Omit<CodeModelGroup, "options">,
    option: CodeModelOption,
  ) => {
    const existing = byId.get(badge.id);
    if (existing) existing.options.push(option);
    else byId.set(badge.id, { ...badge, options: [option] });
  };
  for (const option of options) {
    const family = familyForModelId(option.id);
    if (family) {
      add(
        {
          id: family.match,
          label: family.label,
          iconProvider: "model_gateway",
          iconModelId: family.match,
        },
        option,
      );
      continue;
    }
    const vendor = codeModelVendor(option);
    if (vendor) {
      add(
        {
          id: vendor,
          label: VENDOR_LABELS[vendor] ?? vendor,
          iconProvider: vendor,
        },
        option,
      );
      continue;
    }
    add(
      { id: "other", label: "Other", iconProvider: "openai_compatible" },
      option,
    );
  }

  const rank = (group: CodeModelGroup): number => {
    const vendor = CODE_VENDOR_ORDER.indexOf(group.id as ProviderKind);
    if (vendor !== -1) return vendor;
    if (group.id === "other") return Number.MAX_SAFE_INTEGER;
    return CODE_VENDOR_ORDER.length;
  };
  return [...byId.values()].sort((a, b) => rank(a) - rank(b));
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
