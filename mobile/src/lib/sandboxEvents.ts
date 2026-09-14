/**
 * Humanized one-liners for sandbox event kinds — the projection mg ADR 0044
 * asks watchers to render instead of kind plus a JSON dump.
 *
 * An unknown kind falls back to its own slug with underscores spaced: a
 * gateway newer than this build must never render a blank timeline row.
 */

import type { SandboxEvent } from "./consoleTypes";

function str(
  payload: Record<string, unknown>,
  key: string,
): string | undefined {
  const value = payload[key];
  return typeof value === "string" ? value : undefined;
}

function num(
  payload: Record<string, unknown>,
  key: string,
): number | undefined {
  const value = payload[key];
  return typeof value === "number" ? value : undefined;
}

/**
 * Accounting ticks that recur throughout a run and say nothing a watcher acts
 * on — the spend meter already carries their content. The web console hides
 * the same two kinds.
 */
export const HIDDEN_EVENT_KINDS: ReadonlySet<string> = new Set([
  "spend",
  "daily_budget",
]);

/** A slug from a vocabulary this build does not know, rendered as words. */
export function spacedSlug(slug: string): string {
  return slug.replace(/_/g, " ");
}

export function describeEvent(event: SandboxEvent): string {
  const p = event.payload;
  switch (event.kind) {
    case "spawned":
      return `Spawned (${str(p, "harness") ?? "harness"})`;
    case "awaiting_scheduling":
      return (
        str(p, "message") ??
        str(p, "reason") ??
        "Waiting for the cluster to place the pod"
      );
    case "initializing":
      return "Pod placed — initializing";
    case "bootstrap_started":
      return "Bootstrap started";
    case "repository_cloned":
      return `Cloned ${str(p, "repository") ?? "repository"}`;
    case "workspace_prepared":
      return "Workspace prepared";
    case "harness_configured":
      return `Harness configured (${str(p, "model") ?? "default model"})`;
    case "supervisor_started":
      return "Supervisor started";
    case "turn_started": {
      const turn = num(p, "turn");
      const from = str(p, "source") === "inbox" ? " from your message" : "";
      return `Turn ${turn ?? "?"} started${from}`;
    }
    case "turn_completed": {
      const turn = num(p, "turn");
      const exit = num(p, "exit_code");
      return exit === 0
        ? `Turn ${turn ?? "?"} finished`
        : `Turn ${turn ?? "?"} exited with code ${exit ?? "?"}`;
    }
    case "turn_interrupted":
      return `Turn ${num(p, "turn") ?? "?"} interrupted`;
    case "turn_progress": {
      const minutes = Math.round((num(p, "elapsed_seconds") ?? 0) / 60);
      const band = str(p, "output_band") ?? "unknown";
      return `Turn ${num(p, "turn") ?? "?"} running ${minutes}m — output ${band}`;
    }
    case "message_queued":
      return `Steering message queued (#${num(p, "seq") ?? "?"})${
        p.interrupt ? ", interrupting" : ""
      }`;
    case "message_delivered_mid_turn": {
      const delivered = num(p, "messages") ?? 0;
      const subject =
        delivered === 1 ? "Your message" : `${delivered} messages`;
      return `${subject} reached turn ${num(p, "turn") ?? "?"} while it was running`;
    }
    case "task_output":
      return "Task output delivered";
    case "task_output_skipped":
      return "Task output skipped";
    case "task_undeliverable":
      return "Task undeliverable";
    case "message_undeliverable":
      return `Steering message #${num(p, "seq") ?? "?"} undeliverable`;
    case "messages_undelivered": {
      const pending = num(p, "pending") ?? 0;
      const subject = pending === 1 ? "1 message" : `${pending} messages`;
      return `${subject} left undelivered — the run stopped first`;
    }
    case "task_complete":
      return "Task complete — the run ended itself";
    case "acceptance_met":
      return "Pull request landed — draining, steer to reopen";
    case "possibly_stalled":
      return "Possibly stalled — no activity for a while";
    case "soft_ceiling_warned":
      return p.ceiling === "spend"
        ? "Approaching spend ceiling — wrap-up turn"
        : "Approaching wall-clock ceiling — wrap-up turn";
    case "spend_ceiling_exceeded":
      return "Spend ceiling exceeded — terminating";
    case "user_daily_budget_exceeded":
      return "Daily budget exceeded";
    case "installation_daily_budget_exceeded":
      return "Installation daily budget exceeded";
    case "scheduling_timeout":
      return "Scheduling timed out";
    case "node_landed":
      return "Placed on a node";
    case "pod_provisioned": {
      const incarnation = num(p, "incarnation");
      return incarnation != null && incarnation > 1
        ? `Pod provisioned (incarnation ${incarnation})`
        : "Pod provisioned";
    }
    case "pod_lost":
      return `Pod lost — ${str(p, "reason") ?? "infrastructure"}`;
    case "pod_terminal_condition":
      return str(p, "classification")
        ? `Pod ended: ${str(p, "classification")}`
        : "Pod ended";
    case "grant_revoked":
      return "Grant revoked";
    case "user_disabled":
      return "User disabled";
    case "termination_requested":
      return "Termination requested";
    case "administrator_cancelled":
      return "Cancelled by an administrator";
    case "container_output":
      return "Container output captured";
    case "running":
      return "Running";
    case "completing":
      return "Completing";
    case "completed":
      return "Completed";
    case "failed":
      return `Failed${str(p, "reason") ? ` — ${str(p, "reason")}` : ""}`;
    case "cancelled":
      return "Cancelled";
    case "expired":
      return `Expired${str(p, "reason") ? ` — ${str(p, "reason")}` : ""}`;
    // A bound was reached, not a breakage. The reason names which bound, and
    // the run may well have finished its work first.
    case "ceiling_exceeded":
      return `Ceiling reached${str(p, "reason") ? ` — ${str(p, "reason")}` : ""}`;
    default:
      return spacedSlug(event.kind);
  }
}
