import type {
  HarnessCaps,
  HarnessKind,
  PermissionMode,
} from "../generated/wire";
import type { CodeHarnessOption } from "./api";

export const HARNESS_LABELS: Record<HarnessKind, string> = {
  claude_code: "Claude Code",
  codex: "Codex CLI",
  opencode: "opencode",
  grok: "Grok CLI",
};

export const PERMISSION_MODE_LABELS: Record<PermissionMode, string> = {
  plan: "Plan",
  ask: "Ask",
  auto: "Auto",
  allow: "Allow all",
};

export const PERMISSION_MODE_DESCRIPTIONS: Record<PermissionMode, string> = {
  plan: "Plans the work and writes nothing.",
  ask: "Asks before every tool that changes something.",
  auto: "Decides for itself as it works.",
  allow: "Runs every tool without asking.",
};

const PERMISSION_SCALE: PermissionMode[] = ["plan", "ask", "auto", "allow"];

type ModeCaps = Pick<
  HarnessCaps,
  "plan_mode" | "structured_approvals" | "auto_mode" | "allow_mode"
>;

export function createPermissionModes(caps: ModeCaps): PermissionMode[] {
  const modes: PermissionMode[] = [];
  if (caps.plan_mode === "supported") modes.push("plan");
  if (caps.structured_approvals === "supported") modes.push("ask");
  if (caps.auto_mode === "supported") modes.push("auto");
  if (caps.allow_mode === "supported") modes.push("allow");
  return modes;
}

export function permittedPermissionModes(
  caps: ModeCaps,
  ceiling: PermissionMode | undefined,
): PermissionMode[] {
  const modes = createPermissionModes(caps);
  if (!ceiling) return modes;
  const ceilingRank = PERMISSION_SCALE.indexOf(ceiling);
  return modes.filter(
    (mode) => PERMISSION_SCALE.indexOf(mode) <= ceilingRank,
  );
}

export function defaultCreatePermissionMode(
  modes: readonly PermissionMode[],
): PermissionMode | null {
  return modes.length > 0 ? modes[modes.length - 1] ?? null : null;
}

export function harnessUnavailableReason(
  harness: CodeHarnessOption,
): string | null {
  if (harness.auth_mode === "hosted_unavailable") {
    return "Not available on hosted machines.";
  }
  if (!harness.found) {
    return harness.installable
      ? "Install this harness from Tidebreak desktop first."
      : "Not installed on this machine.";
  }
  if (
    harness.auth_mode === "local_sign_in" &&
    harness.authenticated !== true
  ) {
    return harness.remediation || "Sign in on the machine first.";
  }
  if (createPermissionModes(harness.caps).length === 0) {
    return "This harness cannot start a session.";
  }
  return null;
}

export function harnessCanStartNow(harness: CodeHarnessOption): boolean {
  return harnessUnavailableReason(harness) === null;
}

export function permissionModeWarning(
  mode: PermissionMode,
  caps: ModeCaps,
): string | null {
  if (mode === "allow") {
    return "This harness runs every tool without asking.";
  }
  if (
    mode === "auto" &&
    caps.structured_approvals !== "supported"
  ) {
    return "This harness has no approval channel. Auto runs every action without asking.";
  }
  return null;
}

