/**
 * The small closed vocabularies the console screens render as chips and
 * labels, each mapped to the repo's own status tones.
 *
 * Every map falls through to the raw slug: the gateway versions these
 * vocabularies, so a newer installation can name a value this build has never
 * seen, and showing it beats showing nothing.
 *
 * The tone discipline throughout: `critical` is reserved for malfunction. A
 * run that reached a bound the installation configured did not break, so it
 * warns rather than alarms.
 */

import type {
  AppConnection,
  SandboxContinuationGate,
  SandboxPhase,
  SharedAppStatus,
  SubscriptionLimitState,
} from "./consoleTypes";
import { spacedSlug } from "./sandboxEvents";

// Re-exported so a screen that renders one unknown slug does not have to reach
// into the event module for it.
export { spacedSlug };

/** The tones `StatusPill` in `components/Controls.tsx` renders. */
export type Tone =
  | "neutral"
  | "live"
  | "warning"
  | "info"
  | "success"
  | "critical";

export type Chip = { label: string; tone: Tone };

const PHASE_TONE: Record<string, Tone> = {
  working: "live",
  delivering: "live",
  idle: "neutral",
  awaiting_scheduling: "warning",
  initializing: "warning",
  bootstrapping: "warning",
  preparing_workspace: "warning",
  pending: "warning",
  provisioning: "warning",
  running: "live",
  completing: "neutral",
  completed: "success",
  failed: "critical",
  cancelled: "neutral",
  expired: "warning",
  ceiling_exceeded: "warning",
};

const PHASE_LABEL: Record<string, string> = {
  awaiting_scheduling: "Awaiting scheduling",
  initializing: "Initializing",
  bootstrapping: "Bootstrapping",
  preparing_workspace: "Preparing workspace",
  working: "Working",
  delivering: "Delivering",
  idle: "Idle",
  pending: "Pending",
  provisioning: "Provisioning",
  running: "Running",
  completing: "Completing",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  expired: "Expired",
  ceiling_exceeded: "Ceiling reached",
};

export function phaseChip(phase: SandboxPhase | undefined): Chip {
  const value = phase ?? "pending";
  return {
    label: PHASE_LABEL[value] ?? spacedSlug(value),
    tone: PHASE_TONE[value] ?? "neutral",
  };
}

/**
 * Failure-reason slugs, transcribed from the web console's map. Bounds the
 * installation configured warn; only real malfunction is critical; stops
 * somebody asked for are neutral.
 */
const FAILURE_REASONS: Record<string, Chip> = {
  node_lost: { label: "Node lost", tone: "critical" },
  evicted: { label: "Evicted", tone: "critical" },
  oom_killed: { label: "Out of memory", tone: "critical" },
  bootstrap_input_missing: {
    label: "Bootstrap input missing",
    tone: "critical",
  },
  harness_configure_failed: {
    label: "Harness configuration failed",
    tone: "critical",
  },
  repository_clone_failed: {
    label: "Repository clone failed",
    tone: "critical",
  },
  supervisor_start_failed: {
    label: "Supervisor failed to start",
    tone: "critical",
  },
  backend_failed: { label: "Workload failed", tone: "critical" },
  backend_unavailable: { label: "Backend unavailable", tone: "critical" },
  backend_object_lost: { label: "Backend object lost", tone: "critical" },
  heartbeat_silent: { label: "Heartbeat went silent", tone: "critical" },
  substrate_unproven: { label: "Substrate unproven", tone: "critical" },
  profile_rejected: { label: "Profile rejected", tone: "critical" },
  admission_rejected: { label: "Admission rejected", tone: "critical" },
  profile_unavailable: { label: "Profile unavailable", tone: "critical" },
  credentials_unavailable: {
    label: "Credentials unavailable",
    tone: "critical",
  },
  inference_unavailable: { label: "Inference unavailable", tone: "critical" },
  wall_clock_ceiling: { label: "Wall-clock ceiling", tone: "warning" },
  idle_ceiling: { label: "Idle ceiling", tone: "warning" },
  spend_ceiling_exceeded: { label: "Spend ceiling", tone: "warning" },
  user_daily_budget_exceeded: { label: "Daily budget", tone: "warning" },
  installation_daily_budget_exceeded: {
    label: "Installation daily budget",
    tone: "warning",
  },
  scheduling_timeout: { label: "Scheduling timed out", tone: "warning" },
  grant_revoked: { label: "Grant revoked", tone: "neutral" },
  user_disabled: { label: "User disabled", tone: "neutral" },
  installation_disabled: { label: "Installation disabled", tone: "neutral" },
};

export function failureReasonChip(reason: string): Chip {
  return (
    FAILURE_REASONS[reason] ?? { label: spacedSlug(reason), tone: "neutral" }
  );
}

/** Why the run parked instead of resuming, phrased for the steer decision. */
const CONTINUATION_GATES: Record<string, string> = {
  max_turns: "turn budget used",
  exit_status: "last turn failed",
  turn_mode: "turn mode — waits for steer",
  task_complete: "task complete",
  acceptance: "PR landed — draining",
  soft_ceiling_wrap_up: "ceiling wrap-up done",
};

export function continuationGateLabel(gate: SandboxContinuationGate): string {
  return CONTINUATION_GATES[gate] ?? spacedSlug(gate);
}

/**
 * Connection state → what a member can do about it. Only delegated-OAuth apps
 * are ever anything but ready, and the fix for both unready states is a
 * browser sign-in the gateway hosts — so the copy says where to go, not what
 * broke.
 */
const APP_CONNECTIONS: Record<string, Chip> = {
  ready: { label: "Connected", tone: "success" },
  not_connected: { label: "Not connected", tone: "neutral" },
  authorization_required: { label: "Reconnect in browser", tone: "warning" },
};

export function appConnectionChip(connection: AppConnection): Chip {
  return (
    APP_CONNECTIONS[connection] ?? {
      label: spacedSlug(connection),
      tone: "neutral",
    }
  );
}

/** Protocol slugs are the wire vocabulary; these are the readable halves. */
const PROTOCOL_LABELS: Record<string, string> = {
  anthropic_messages: "Anthropic",
  openai_responses: "OpenAI",
};

export function protocolLabel(protocol: string): string {
  return PROTOCOL_LABELS[protocol] ?? spacedSlug(protocol);
}

/**
 * Shared-app status → chip. Disabled is the team's stop button — reachable by
 * nobody, including the author — so it reads as a state to notice rather than
 * a neutral one.
 */
const SHARED_APP_STATUS: Record<string, Chip> = {
  draft: { label: "Draft", tone: "neutral" },
  published: { label: "Published", tone: "success" },
  disabled: { label: "Disabled", tone: "warning" },
};

export function sharedAppStatusChip(status: SharedAppStatus): Chip {
  return (
    SHARED_APP_STATUS[status] ?? { label: spacedSlug(status), tone: "neutral" }
  );
}

/**
 * Subscription limit state → chip. These are independent facts about an
 * account rather than a state machine, and none of them is a malfunction: a
 * throttled subscription is the provider enforcing a plan, so it warns.
 */
const SUBSCRIPTION_LIMIT_STATE: Record<string, Chip> = {
  available: { label: "Available", tone: "success" },
  cooling_down: { label: "Cooling down", tone: "warning" },
  exhausted_reauth: { label: "Reconnect needed", tone: "warning" },
};

export function subscriptionLimitChip(state: SubscriptionLimitState): Chip {
  return (
    SUBSCRIPTION_LIMIT_STATE[state] ?? {
      label: spacedSlug(state),
      tone: "neutral",
    }
  );
}

/** Cost-limit scope vocabulary as a member reads it — "You" is your own cap. */
const SCOPE_LABELS: Record<string, string> = {
  installation: "Installation",
  user: "You",
  team: "Team",
  gateway_model: "Model",
  connected_app: "App",
};

export function limitScopeLabel(scopeType: string): string {
  return SCOPE_LABELS[scopeType] ?? spacedSlug(scopeType);
}

const WINDOW_LABELS: Record<string, string> = {
  daily: "Daily",
  weekly: "Weekly",
  monthly: "Monthly",
};

export function limitWindowLabel(window: string): string {
  return WINDOW_LABELS[window] ?? spacedSlug(window);
}

/** Workstation traffic and detached-agent traffic, named rather than slugged. */
export function executionKindLabel(kind: string): string {
  if (kind === "command") {
    return "Workstation";
  }
  if (kind === "sandbox") {
    return "Sandbox";
  }
  return spacedSlug(kind);
}

/**
 * A count at a glance: 2.6B, 6.2M, 940K. Token totals run to ten digits, which
 * no phone-width tile can show — and nobody reads the last seven of them.
 */
export function formatCompact(value: number): string {
  return new Intl.NumberFormat(undefined, {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(value);
}

export function formatCount(value: number): string {
  return value.toLocaleString();
}
