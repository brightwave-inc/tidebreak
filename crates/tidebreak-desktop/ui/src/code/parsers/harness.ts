import {
  isFiniteNumber,
  isMember,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CapLevel,
  HarnessCaps,
  HarnessAuthMode,
  HarnessDoctorEntry,
  HarnessDoctorReport,
  HarnessKind,
  HarnessTier,
  ReasoningEffort,
  CodeHarnessInstallSnapshot,
} from "../../api/types";
import type {
  HarnessCaps as WireHarnessCaps,
  HarnessDoctorEntry as WireHarnessDoctorEntry,
  HarnessDoctorReport as WireHarnessDoctorReport,
  HarnessModelSource as WireHarnessModelSource,
  HarnessUpdateChannel as WireHarnessUpdateChannel,
  CodeHarnessInstallSnapshot as WireCodeHarnessInstallSnapshot,
} from "../../generated/wire";
import {
  lineText,
  optionalLine,
  blockText,
  optionalBlock,
  rawText,
  HARNESS_KINDS,
  REASONING_EFFORTS,
} from "./shared";

const HARNESS_TIERS = new Set<HarnessTier>([
  "reference",
  "secondary",
  "tertiary",
  "best_effort",
]);
const HARNESS_AUTH_MODES = new Set<HarnessAuthMode>([
  "local_sign_in",
  "gateway_managed",
  "gateway_relay",
]);
const HARNESS_UPDATE_CHANNELS = new Set<WireHarnessUpdateChannel>([
  "pinned",
  "latest",
]);
const HARNESS_MODEL_SOURCES = new Set<WireHarnessModelSource>([
  "harness",
  "model_gateway",
]);
const CAP_LEVELS = new Set<CapLevel>(["supported", "unsupported", "unknown"]);

export function parseCodeHarnessInstall(
  value: unknown,
): CodeHarnessInstallSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeHarnessInstallSnapshot>(value, [
      "kind",
      "version",
      "phase",
      "done",
      "error",
    ]) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    !lineText(value.phase) ||
    typeof value.done !== "boolean" ||
    !optionalLine(value.version) ||
    !optionalBlock(value.error)
  ) {
    return null;
  }
  return {
    kind: value.kind,
    phase: value.phase,
    done: value.done,
    ...(value.version !== undefined ? { version: value.version } : {}),
    ...(value.error !== undefined ? { error: value.error } : {}),
  };
}

export type ParsedHarnessModel = {
  id: string;
  label: string;
  default: boolean;
  reasoning_efforts: ReasoningEffort[];
  fast_mode: boolean;
};

export type ParsedHarnessModelList = {
  kind: HarnessKind;
  /** Missing means an older server whose listing was always native. */
  source?: WireHarnessModelSource;
  models: ParsedHarnessModel[];
  reasoning_efforts: ReasoningEffort[];
};

function parseEfforts(value: unknown): ReasoningEffort[] {
  // A server that predates the field, or a level this build cannot label,
  // narrows the offer rather than failing the whole list.
  return Array.isArray(value)
    ? value.filter((level): level is ReasoningEffort =>
        isMember(level, REASONING_EFFORTS),
      )
    : [];
}

export function parseHarnessModelList(
  value: unknown,
): ParsedHarnessModelList | null {
  if (
    !isRecord(value) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    !Array.isArray(value.models)
  ) {
    return null;
  }
  const source =
    value.source === undefined
      ? "harness"
      : isMember(value.source, HARNESS_MODEL_SOURCES)
        ? value.source
        : null;
  if (source === null) return null;
  const models: ParsedHarnessModel[] = [];
  for (const item of value.models) {
    if (
      !isRecord(item) ||
      !lineText(item.id) ||
      !lineText(item.label) ||
      typeof item.default !== "boolean"
    ) {
      return null;
    }
    models.push({
      id: item.id,
      label: item.label,
      default: item.default,
      reasoning_efforts: parseEfforts(item.reasoning_efforts),
      // A server that predates the field offers no fast mode, which is the
      // same thing a row without the tier says.
      fast_mode: item.fast_mode === true,
    });
  }
  return {
    kind: value.kind,
    source,
    models,
    reasoning_efforts: parseEfforts(value.reasoning_efforts),
  };
}

export function parseHarnessDoctorReport(
  value: unknown,
): HarnessDoctorReport | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessDoctorReport>(value, [
      "harnesses",
      "update_channel",
    ]) ||
    !Array.isArray(value.harnesses) ||
    (value.update_channel !== undefined &&
      !isMember(value.update_channel, HARNESS_UPDATE_CHANNELS))
  ) {
    return null;
  }
  const harnesses: HarnessDoctorEntry[] = [];
  for (const entry of value.harnesses) {
    const parsed = parseHarnessDoctorEntry(entry);
    if (!parsed) return null;
    harnesses.push(parsed);
  }
  // A server that predates the channel drives its pins and nothing else.
  return { harnesses, update_channel: value.update_channel ?? "pinned" };
}

export function parseHarnessDoctorEntry(
  value: unknown,
): HarnessDoctorEntry | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessDoctorEntry>(value, [
      "kind",
      "found",
      "installable",
      "path",
      "version",
      "tier",
      "caps",
      "commands",
      "authenticated",
      "auth_mode",
      "remediation",
      "stderr",
      "unrecognized_event_count",
      "relaunch_composes_permission_mode",
      "pinned_version",
      "managed_version",
      "latest_version",
      "update_available",
      "sign_in_command",
      "review_blocked",
    ]) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    typeof value.found !== "boolean" ||
    !optionalLine(value.path) ||
    !optionalLine(value.sign_in_command) ||
    !optionalBlock(value.review_blocked) ||
    !optionalLine(value.version) ||
    !optionalLine(value.pinned_version) ||
    !optionalLine(value.managed_version) ||
    !optionalLine(value.latest_version) ||
    (value.update_available !== undefined &&
      typeof value.update_available !== "boolean") ||
    !isMember(value.tier, HARNESS_TIERS) ||
    (value.authenticated !== undefined &&
      typeof value.authenticated !== "boolean") ||
    (value.auth_mode !== undefined &&
      !isMember(value.auth_mode, HARNESS_AUTH_MODES)) ||
    !blockText(value.remediation) ||
    !rawText(value.stderr) ||
    !isFiniteNumber(value.unrecognized_event_count) ||
    (value.relaunch_composes_permission_mode !== undefined &&
      typeof value.relaunch_composes_permission_mode !== "boolean")
  ) {
    return null;
  }
  const caps = parseHarnessCaps(value.caps);
  if (!caps) return null;
  const commands = parseHarnessCommands(value.commands);
  if (!commands) return null;
  return {
    kind: value.kind,
    found: value.found,
    // A server that predates the field offers no on-demand download, which
    // is what this build did before the pin became lazy.
    installable: value.installable === true,
    // Ditto the hosted doctor: a server that predates it knows only the
    // local sign-in probe, and that is the local answer.
    auth_mode: value.auth_mode ?? "local_sign_in",
    // Engines historically recomposed the mode on relaunch; a server that
    // predates the field still behaves that way.
    relaunch_composes_permission_mode:
      value.relaunch_composes_permission_mode !== false,
    tier: value.tier,
    caps,
    remediation: value.remediation,
    stderr: value.stderr,
    unrecognized_event_count: value.unrecognized_event_count,
    commands,
    // A server that predates the update channel never has one to offer.
    update_available: value.update_available === true,
    ...(value.path !== undefined ? { path: value.path } : {}),
    ...(value.version !== undefined ? { version: value.version } : {}),
    ...(value.pinned_version !== undefined
      ? { pinned_version: value.pinned_version }
      : {}),
    ...(value.managed_version !== undefined
      ? { managed_version: value.managed_version }
      : {}),
    ...(value.latest_version !== undefined
      ? { latest_version: value.latest_version }
      : {}),
    ...(value.authenticated !== undefined
      ? { authenticated: value.authenticated }
      : {}),
    // A server that predates the Sign in action offers none.
    ...(value.sign_in_command
      ? { sign_in_command: value.sign_in_command }
      : {}),
    ...(value.review_blocked !== undefined
      ? { review_blocked: value.review_blocked }
      : {}),
  };
}

function parseHarnessCommands(
  value: unknown,
): { name: string; description: string }[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value)) return null;
  const commands: { name: string; description: string }[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !lineText(item.name) ||
      item.name.length === 0 ||
      !blockText(item.description)
    ) {
      return null;
    }
    commands.push({ name: item.name, description: item.description });
  }
  return commands;
}

function parseHarnessCaps(value: unknown): HarnessCaps | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessCaps>(value, [
      "resume",
      "streaming_deltas",
      "structured_approvals",
      "mid_turn_steering",
      "plan_mode",
      "auto_mode",
      "allow_mode",
      "reasoning_levels",
      "native_file_change_events",
      "native_interrupt",
      "image_input",
      "slash_commands",
      "durable_parks",
      "user_questions",
      "standing_grants",
      "mid_turn_resume",
      "transcript",
      "memory_loopback",
    ]) ||
    !isMember(value.resume, CAP_LEVELS) ||
    !isMember(value.streaming_deltas, CAP_LEVELS) ||
    !isMember(value.structured_approvals, CAP_LEVELS) ||
    !isMember(value.mid_turn_steering, CAP_LEVELS) ||
    !isMember(value.plan_mode, CAP_LEVELS) ||
    !isMember(value.auto_mode, CAP_LEVELS) ||
    !isMember(value.allow_mode, CAP_LEVELS) ||
    !isMember(value.reasoning_levels, CAP_LEVELS) ||
    !isMember(value.native_file_change_events, CAP_LEVELS) ||
    !isMember(value.native_interrupt, CAP_LEVELS) ||
    !isMember(value.image_input, CAP_LEVELS) ||
    !isMember(value.slash_commands, CAP_LEVELS) ||
    !isMember(value.durable_parks, CAP_LEVELS) ||
    !isMember(value.user_questions, CAP_LEVELS) ||
    !isMember(value.standing_grants, CAP_LEVELS) ||
    !isMember(value.mid_turn_resume, CAP_LEVELS) ||
    !isMember(value.transcript, CAP_LEVELS) ||
    !isMember(value.memory_loopback, CAP_LEVELS)
  ) {
    return null;
  }
  return {
    resume: value.resume,
    streaming_deltas: value.streaming_deltas,
    structured_approvals: value.structured_approvals,
    mid_turn_steering: value.mid_turn_steering,
    plan_mode: value.plan_mode,
    auto_mode: value.auto_mode,
    allow_mode: value.allow_mode,
    reasoning_levels: value.reasoning_levels,
    native_file_change_events: value.native_file_change_events,
    native_interrupt: value.native_interrupt,
    image_input: value.image_input,
    slash_commands: value.slash_commands,
    durable_parks: value.durable_parks,
    user_questions: value.user_questions,
    standing_grants: value.standing_grants,
    mid_turn_resume: value.mid_turn_resume,
    transcript: value.transcript,
    memory_loopback: value.memory_loopback,
  };
}
