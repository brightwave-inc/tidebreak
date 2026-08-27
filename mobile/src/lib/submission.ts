import type { CodeTurnSnapshot, QueuedCodeTurn } from "../generated/wire";
import { MachineRequestError } from "./machine";

export type SubmissionFailure = {
  message: string;
  deliveryUnknown: boolean;
};

export type AmbiguousCodeSubmission = {
  message: string;
  knownTurnIds: ReadonlySet<string>;
  knownQueuedIds: ReadonlySet<string>;
};

export type SessionActionState = {
  submittingTurn: boolean;
  steering: boolean;
  interrupting: boolean;
  refreshing: boolean;
  deliveryUnknown: boolean;
};

export function sessionActionAvailability(state: SessionActionState) {
  // POST /turns stays open until an idle turn settles. That pending request
  // must block a second follow-up without blocking steer or interrupt.
  const controlsBlocked =
    state.steering ||
    state.interrupting ||
    state.refreshing ||
    state.deliveryUnknown;
  return {
    canChangeMode: !controlsBlocked,
    canSteer: !controlsBlocked,
    canFollowUp: !controlsBlocked && !state.submittingTurn,
    canInterrupt: !controlsBlocked,
  };
}

export function clearDeliveredDraft(current: string, delivered: string): string {
  return current.trim() === delivered ? "" : current;
}

export function restoreUndeliveredDraft(
  current: string,
  submitted: string,
): string {
  return current.trim().length === 0 ? submitted : current;
}

export function codeSubmissionWasAccepted(
  attempt: AmbiguousCodeSubmission,
  turns: readonly CodeTurnSnapshot[],
  queued: readonly QueuedCodeTurn[],
): boolean {
  return (
    turns.some(
      (turn) =>
        !attempt.knownTurnIds.has(turn.id) && turn.user_input === attempt.message,
    ) ||
    queued.some(
      (turn) =>
        !attempt.knownQueuedIds.has(turn.id) && turn.message === attempt.message,
    )
  );
}

/**
 * HTTP failures are definitive. A transport or parser failure after dispatch
 * is ambiguous because code turns do not carry a client idempotency key.
 */
export function submissionFailure(
  error: unknown,
  requestDispatched: boolean,
): SubmissionFailure {
  if (error instanceof MachineRequestError || !requestDispatched) {
    return {
      message:
        error instanceof Error ? error.message : "The request could not be sent.",
      deliveryUnknown: false,
    };
  }
  return {
    message:
      "The connection ended before Tidebreak confirmed delivery. Refresh before sending again.",
    deliveryUnknown: true,
  };
}
