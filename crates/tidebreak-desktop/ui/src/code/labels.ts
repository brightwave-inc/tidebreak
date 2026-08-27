import type {
  Attention,
  PermissionMode,
  CodeSessionLifecycle,
  CodeWorkspaceStatus,
  FenceReason,
  HarnessAuthMode,
  HarnessCaps,
  HarnessKind,
  HarnessTier,
  ModelInfo,
  ProviderKind,
  ReasoningEffort,
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

/**
 * Lifecycle cluster tooltip: posture, harness + version, and historical
 * engine events the adapter could not classify (decision 0031).
 */
export function sessionLifecycleTooltip(input: {
  lifecycle: CodeSessionLifecycle;
  harness: HarnessKind;
  version?: string;
  unrecognizedEventCount: number;
  /** Precise live work, when a running digest can provide it. */
  runningLabel?: string;
}): string {
  const harness = input.version
    ? `${HARNESS_LABELS[input.harness]} ${input.version}`
    : HARNESS_LABELS[input.harness];
  const lifecycle =
    input.lifecycle === "running" && input.runningLabel
      ? input.runningLabel
      : LIFECYCLE_LABELS[input.lifecycle];
  const parts = [lifecycle, harness];
  if (input.unrecognizedEventCount > 0) {
    const count = input.unrecognizedEventCount;
    const noun = count === 1 ? "event" : "events";
    parts.push(
      `${count} unrecognized engine ${noun} recorded — transcript may be incomplete`,
    );
  }
  return parts.join(" · ");
}

export const WORKSPACE_STATUS_LABELS: Record<CodeWorkspaceStatus, string> = {
  creating: "Creating",
  setup_failed: "Setup failed",
  active: "Active",
  archiving: "Archiving",
  archived: "Archived",
  released: "Released",
};

export const PERMISSION_MODE_LABELS: Record<PermissionMode, string> = {
  plan: "Plan",
  ask: "Ask",
  auto: "Auto",
  allow: "Allow all",
};

/** Shown when the selected harness cannot honor Ask or Auto. */
export const PERMISSION_MODE_UNAVAILABLE_REASON =
  "this harness cannot honor that mode";

/**
 * Shown where the mode is fixed for the life of the surface — a create form
 * that has already posted, a read-only view of someone else's session, or a
 * live engine that fixes its posture when the session starts (opencode).
 */
export const SESSION_PERMISSION_MODE_LOCKED =
  "Set when the session started — start a new session to change it";

/** Create-time hint when the selected engine cannot change mode after start. */
export const CREATE_PERMISSION_MODE_FIXED =
  "Mode is fixed once the session starts";

/** What each posture does, in one line, for the surfaces that state it. */
export const PERMISSION_MODE_POSTURES: Record<PermissionMode, string> = {
  plan: "Plans the work and writes nothing",
  ask: "Asks before every tool that changes something",
  // No claim about escalation: whether Auto has anywhere to ask is the
  // engine's capability, not the mode's (see [`autoIsUnsupervised`]).
  auto: "Decides for itself as it works",
  allow: "Runs every tool without asking",
};

/** Stated wherever unsupervised Auto is offered (decision 0038). */
export const UNSUPERVISED_AUTO_NOTE =
  "This engine has no approval channel: in Auto, every action runs without asking.";

/** Stated wherever Allow is offered (decision 0039). */
export const ALLOW_ALL_NOTE =
  "This engine's permission system is off: every action runs without asking.";

/**
 * The statement a surface shows next to the permission control when the
 * posture on offer runs with nobody to ask (decisions 0038, 0039).
 *
 * `null` for a posture that escalates, so the statement appears only where it
 * changes what the reader should expect: Allow all anywhere, and Auto on an
 * engine whose Auto is unsupervised.
 */
export function unsupervisedModeStatement(
  mode: PermissionMode,
  autoUnsupervised: boolean,
): string | null {
  if (mode === "allow") return ALLOW_ALL_NOTE;
  if (mode === "auto" && autoUnsupervised) return UNSUPERVISED_AUTO_NOTE;
  return null;
}

/** The header chip's tooltip: the posture, named and spelled out. */
export function sessionPermissionModeTooltip(mode: PermissionMode): string {
  return `Permissions: ${PERMISSION_MODE_LABELS[mode]}\n${PERMISSION_MODE_POSTURES[mode]}`;
}

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

/**
 * True when this engine can serve a session right now.
 *
 * On a gateway-hosted machine the relay-covered engines are ready without a
 * local sign-in (decision 71), and the uncovered ones can never be; the
 * `auth_mode` the server reports decides, and `authenticated` — the local
 * probe observation — does not.
 */
export function isHarnessReady(entry: {
  found: boolean;
  authenticated?: boolean;
  auth_mode?: HarnessAuthMode;
}): boolean {
  const mode = entry.auth_mode ?? "local_sign_in";
  if (mode === "gateway_relay") return entry.found;
  if (mode === "hosted_unavailable") return false;
  return entry.found && entry.authenticated === true;
}

/** True when create can post at least one permission mode this engine honors. */
export function harnessHonorsAnyCreateMode(entry: { caps: ModeCaps }): boolean {
  return createPermissionModes(entry.caps).length > 0;
}

/**
 * True when this engine is not on disk yet but Tidebreak ships a pin it can
 * download.
 *
 * Every engine used to have to be installed before any of them could be used,
 * because the doctor's only install control fetched all four. A missing pin is
 * now a wait, not a fault: pick the engine and the download starts.
 */
export function harnessNeedsDownload(entry: {
  found: boolean;
  installable: boolean;
}): boolean {
  return !entry.found && entry.installable;
}

/**
 * Why a picker row is not selectable. Ready rows return null, and so does a
 * row that only needs downloading — see [`harnessNeedsDownload`].
 * Versions, paths, and capability names stay on the doctor.
 */
export function harnessUnusableReason(entry: {
  found: boolean;
  installable: boolean;
  authenticated?: boolean;
  auth_mode?: HarnessAuthMode;
  remediation?: string;
  caps: ModeCaps;
}): string | null {
  const mode = entry.auth_mode ?? "local_sign_in";
  if (mode === "hosted_unavailable") {
    return "Not available on hosted machines yet";
  }
  if (!entry.found && !entry.installable) return "Not installed";
  // A relay-covered engine needs no sign-in on a hosted machine, so the
  // local probe observation is not a gate there either.
  if (mode === "local_sign_in") {
    if (entry.authenticated === false) return "Sign in via your terminal";
    if (entry.found && entry.authenticated === undefined) {
      return "Unverified — sign in via your terminal";
    }
  }
  if (!harnessHonorsAnyCreateMode(entry)) return "Not available yet";
  return null;
}

/** True when this engine can be started right now, with nothing to wait for. */
export function harnessCanStartNow(entry: {
  found: boolean;
  installable: boolean;
  authenticated?: boolean;
  auth_mode?: HarnessAuthMode;
  remediation?: string;
  caps: ModeCaps;
}): boolean {
  return entry.found && !harnessUnusableReason(entry);
}

/**
 * True when this engine's Auto runs with nobody to ask (decision 0038):
 * it has an auto posture but no approval channel to escalate through.
 */
export function autoIsUnsupervised(caps: ModeCaps): boolean {
  return (
    caps.auto_mode === "supported" && caps.structured_approvals !== "supported"
  );
}

/**
 * Create default: the most autonomous posture the engine honors, walking
 * Allow → Auto → Ask → Plan (decision 0039, amended 2026-08-18). Approving
 * every step of a fresh session cost more than it caught, so create starts
 * where the work runs. The mode is never silent: whichever surface offers it
 * states the posture next to the control (decisions 0033, 0038).
 */
export function defaultCreatePermissionMode(caps: ModeCaps): PermissionMode {
  if (caps.allow_mode === "supported") return "allow";
  if (caps.auto_mode === "supported") return "auto";
  if (caps.structured_approvals === "supported") return "ask";
  return "plan";
}

/** The modes create may post for this engine, each on its own flag. */
export function createPermissionModes(caps: ModeCaps): PermissionMode[] {
  const modes: PermissionMode[] = [];
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
  /**
   * Levels this row accepts, when the engine states a ladder per model.
   * Absent falls back to the engine's own ladder; see [`effortLadder`].
   */
  reasoning_efforts?: readonly ReasoningEffort[];
  /**
   * Whether this row serves the engine's fast mode. Absent reads as no: a row
   * the engine did not list, or a server that predates the field, cannot
   * promise a tier, and offering one it will not honor is worse than hiding
   * the control.
   */
  fast_mode?: boolean;
};

/**
 * The levels to offer for one row of a code-mode picker.
 *
 * A row the engine listed itself may narrow the offer — Codex advertises a
 * different ladder per model, and only some of its rows reach the top rung.
 * Anything else (a gateway catalog row, a session still on a model the engine
 * has dropped) falls back to the engine's ladder, which is the outer bound.
 */
export function effortLadder(
  option: CodeModelOption | undefined,
  engine: readonly ReasoningEffort[],
): readonly ReasoningEffort[] {
  const row = option?.reasoning_efforts ?? [];
  return row.length > 0 ? row : engine;
}

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
      // Deliberately not `model.reasoning_efforts`: that is the chat catalog's
      // ladder for this model, and a code session's ladder belongs to the
      // engine driving it. Claude Code reaches a rung the chat route has no
      // equivalent for, and grok stops below one it does. The caller supplies
      // the engine's ladder instead.
    }));
}

export function harnessCodeModels(
  listed: readonly {
    id: string;
    label: string;
    default?: boolean;
    reasoning_efforts?: readonly ReasoningEffort[];
    fast_mode?: boolean;
  }[],
  kind: HarnessKind,
): CodeModelOption[] {
  const source = HARNESS_LABELS[kind];
  return listed.map((option) => ({
    id: option.id,
    label: prettyCodeModelLabel(option.label || option.id),
    source,
    vendor: vendorForModelId(option.id),
    default: option.default,
    reasoning_efforts: option.reasoning_efforts,
    fast_mode: option.fast_mode,
  }));
}

/** Whether this engine requires the exact model IDs from its own listing. */
export function requiresHarnessModelIds(kind: HarnessKind): boolean {
  return kind === "opencode" || kind === "grok";
}

/**
 * Pick the catalog whose identifiers the engine accepts.
 *
 * OpenCode and Grok need the provider-qualified ids from their own model
 * listings; chat's gateway catalog carries a bare upstream id instead. Other
 * engines keep the gateway catalog when it exists because their adapters
 * accept those ids directly and the catalog carries the entitled display rows.
 */
/**
 * Copy engine-owned caps from the harness listing onto gateway display rows.
 *
 * Gateway catalog rows never carry `fast_mode` (or a code-engine effort
 * ladder). The in-workspace picker still fetches the harness listing for
 * those fields and joins by model id, so a gateway-selected claude-opus-5
 * can show the fast toggle.
 */
function overlayHarnessModelCaps(
  gateway: readonly CodeModelOption[],
  native: readonly CodeModelOption[],
): CodeModelOption[] {
  if (native.length === 0) return [...gateway];
  const byId = new Map(native.map((option) => [option.id, option]));
  return gateway.map((row) => {
    const listed = byId.get(row.id);
    if (!listed) return row;
    return {
      ...row,
      ...(listed.reasoning_efforts && listed.reasoning_efforts.length > 0
        ? { reasoning_efforts: listed.reasoning_efforts }
        : {}),
      ...(listed.fast_mode ? { fast_mode: true } : {}),
    };
  });
}

export function preferredCodeModels(
  kind: HarnessKind,
  native: readonly CodeModelOption[],
  gateway: readonly CodeModelOption[],
): CodeModelOption[] {
  if (requiresHarnessModelIds(kind) && native.length > 0) return [...native];
  if (gateway.length > 0) return overlayHarnessModelCaps(gateway, native);
  return [...native];
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
    .map((part) =>
      part.toLowerCase() === "gpt"
        ? "GPT"
        : part[0]?.toUpperCase() + part.slice(1),
    )
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
    case "idle":
      return "Idle";
    case "fenced":
      return "Fenced";
    case "manual":
      return attention.state.note || "Pinned";
  }
}

/**
 * Richer badge tooltip: idle seconds, how a need was detected, the pin note.
 * The visible label stays short.
 */
export function attentionTooltip(attention: Attention): string {
  switch (attention.state.type) {
    case "stalled":
      return `Stalled · idle ${attention.state.idle_secs}s`;
    case "needs_you":
      return `${attentionLabel(attention)} · ${attention.state.source}`;
    case "manual":
      return attention.state.note
        ? `Pinned · ${attention.state.note}`
        : "Pinned";
    default:
      return attentionLabel(attention);
  }
}
