/**
 * The vocabulary the sandbox screens filter, sort, and label by.
 *
 * Kept pure and apart from the screens for the same reason `sections.ts` is:
 * these are the judgments that must not drift between the list, the detail,
 * and the hub's Now strip.
 */

import type {
  SandboxConcurrencyView,
  SandboxState,
  SandboxView,
} from "./consoleTypes";
import { isTerminal } from "./consoleTypes";

/**
 * Every lifecycle state this build knows, in the domain's declaration order. A
 * newer gateway may send one that is not here; see `isKnownState`.
 */
export const SANDBOX_STATES = [
  "pending",
  "provisioning",
  "running",
  "completing",
  "completed",
  "failed",
  "cancelled",
  "expired",
  "ceiling_exceeded",
] as const;

const KNOWN_STATES: ReadonlySet<string> = new Set(SANDBOX_STATES);

/** Whether this build recognizes the state, i.e. whether any chip covers it. */
export function isKnownState(state: SandboxState): boolean {
  return KNOWN_STATES.has(state);
}

export type SandboxStatusGroupKey = "live" | "done" | "stopped" | "failed";

export type SandboxStatusGroup = {
  key: SandboxStatusGroupKey;
  label: string;
  states: readonly SandboxState[];
};

/**
 * The four buckets the sandbox list filters by.
 *
 * Nine chips is more vocabulary than the question deserves. Someone scanning
 * this list is asking one of four things — is it still going, did it finish,
 * was it stopped, did it break — and the distinctions inside a bucket
 * (`pending` vs `provisioning`) are ones the row's own phase badge already
 * makes, at the point where they matter.
 *
 * `failed` sits alone because it means malfunction and nothing else. A run
 * that reached a bound the installation configured is `expired` or
 * `ceiling_exceeded`, and those belong with the deliberate stops: filing them
 * under Failed would report the system working exactly as configured as though
 * something had broken.
 */
export const SANDBOX_STATUS_GROUPS: readonly SandboxStatusGroup[] = [
  {
    key: "live",
    label: "Live",
    states: ["pending", "provisioning", "running", "completing"],
  },
  { key: "done", label: "Done", states: ["completed"] },
  {
    key: "stopped",
    label: "Stopped",
    states: ["cancelled", "expired", "ceiling_exceeded"],
  },
  { key: "failed", label: "Failed", states: ["failed"] },
];

export const ALL_STATUS_GROUP_KEYS: readonly SandboxStatusGroupKey[] =
  SANDBOX_STATUS_GROUPS.map((group) => group.key);

/**
 * The states the selected groups cover, or `null` for "every state".
 *
 * Selecting every group is not the same request as naming all nine states:
 * only the unfiltered read can return a state this build has never heard of,
 * because the server can only answer with states the client knew to ask for.
 * `null` is how the caller says every state, and it is what the query layer
 * turns into an omitted parameter.
 */
export function statesForGroups(
  selected: readonly SandboxStatusGroupKey[],
): SandboxState[] | null {
  if (selected.length >= ALL_STATUS_GROUP_KEYS.length) {
    return null;
  }
  return SANDBOX_STATUS_GROUPS.filter((group) =>
    selected.includes(group.key),
  ).flatMap((group) => [...group.states]);
}

/**
 * Whether a row belongs in the current selection, applied to what came back.
 *
 * The server-side filter is an optimization of the fetch window, not the
 * source of truth: `states` is additive, so a gateway older than it ignores
 * the parameter and answers unfiltered, and a client that trusted the request
 * would render terminal runs under a Live-only selection.
 *
 * A state this build does not know passes every selection rather than none.
 * Hiding a run because the gateway is newer than the app is the worse failure:
 * the run exists, it is the user's, and no chip can be taught to show it.
 */
export function matchesStatusSelection(
  state: SandboxState,
  selected: readonly SandboxStatusGroupKey[],
): boolean {
  if (!isKnownState(state)) {
    return true;
  }
  const states = statesForGroups(selected);
  return states === null || states.includes(state);
}

/**
 * The group that covers this state, or `undefined` for one this build does not
 * know. Used for counting, where "matches every selection" — the right answer
 * for filtering an unknown state — would tally it under every chip.
 */
export function groupOfState(
  state: SandboxState,
): SandboxStatusGroupKey | undefined {
  return SANDBOX_STATUS_GROUPS.find((group) => group.states.includes(state))
    ?.key;
}

/**
 * Whether the run has stopped and will not restart without *this reader*.
 *
 * `may_resume: false` is the gateway saying a turn ended and no gate let the
 * next one start — the run is parked on its `continuation_gate` until someone
 * steers it. That is the only row state where nothing happens until the user
 * acts, so it sorts to the top.
 *
 * Owner-scoped, because steering is owner-only. `viewerId` is required rather
 * than optional so that a caller which has not resolved the reader yet has to
 * say so, and gets `false` — no row claims to be waiting on a reader the
 * screen cannot identify.
 */
export function needsAttention(
  sandbox: SandboxView,
  viewerId: string | undefined,
): boolean {
  if (viewerId === undefined || sandbox.user_id !== viewerId) {
    return false;
  }
  return !isTerminal(sandbox.state) && sandbox.may_resume === false;
}

/**
 * How much of each concurrency ceiling is live, and how much of that is
 * waiting for a node.
 *
 * Each total carries its own waiting count rather than one trailing figure the
 * reader has to attribute: they guess wrong in the case that matters — your
 * own slots all placed while somebody else's sit unschedulable reads as though
 * your quota is the wait.
 */
export function concurrencyOccupancy(view: SandboxConcurrencyView): string {
  const userWaiting =
    view.user_awaiting_scheduling > 0
      ? `, ${view.user_awaiting_scheduling} of them waiting for a node`
      : "";
  const installationWaiting =
    view.installation_awaiting_scheduling > 0
      ? `, ${view.installation_awaiting_scheduling} of them waiting for a node`
      : "";
  return `${view.user_running} of ${view.user_limit} live for you${userWaiting}. ${view.installation_running} of ${view.installation_limit} live across the installation${installationWaiting}.`;
}

/**
 * The cluster's own word on the slots it has not placed, or `undefined` when
 * everything live is placed and there is nothing to explain. A cluster with
 * nowhere to put a pod is not a per-account fact, so this follows both totals
 * rather than either one.
 */
export function schedulingPressureNote(
  view: SandboxConcurrencyView,
): string | undefined {
  const schedulable = view.schedulable;
  if (schedulable && schedulable.available === 0) {
    const classLabel = schedulable.limiting_class
      ? ` (${schedulable.limiting_class.replace(/_/g, " ")})`
      : "";
    const scaling =
      schedulable.autoscaling_active === true
        ? " Autoscaling is adding capacity."
        : schedulable.autoscaling_active === false
          ? " Autoscaling is not adding capacity."
          : "";
    return `No schedulable capacity${classLabel}.${scaling} Retry in ${schedulable.retry_after_seconds} seconds.`;
  }
  if (view.installation_awaiting_scheduling <= 0) {
    return undefined;
  }
  const reported = view.scheduling_pressure
    ? `The cluster reports ${view.scheduling_pressure.reason} on what it has not placed, so those`
    : "Those";
  return `${reported} slots free when the cluster finds capacity, not when a task finishes.`;
}

/** Whether a row's text matches a free-text filter; empty matches everything. */
export function matchesFilter(
  filter: string,
  ...fields: (string | null | undefined)[]
): boolean {
  const needle = filter.trim().toLowerCase();
  if (needle.length === 0) {
    return true;
  }
  return fields.some((field) => field?.toLowerCase().includes(needle) === true);
}
