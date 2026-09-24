import type {
  HarnessCaps,
  HarnessDoctorEntry,
  HarnessKind,
  PermissionMode,
} from "../../api/types";
import { workspaceHarnesses } from "../labels";

/**
 * Which engines can review a workspace's changes, and which one the form
 * offers first.
 *
 * A review runs read-only: in the engine's plan mode where it has one, or in
 * Ask with every request refused where it has approvals Tidebreak can refuse
 * instead. An engine with neither is listed but cannot be chosen. So is one
 * the server says cannot review read-only, such as Grok CLI, which can't turn
 * off network access, and one that is not installed or not signed in: a
 * review never waits on a download or a sign-in, and says why an engine is
 * unavailable.
 *
 * The first choice is an engine other than the one that wrote the changes: a
 * second opinion is the point. Only when no other engine is ready does the
 * form fall back to the author's own engine, and it says so.
 */

/** The permission mode an engine reviews in, or null when it cannot. */
export function reviewPermissionMode(
  caps: Pick<HarnessCaps, "plan_mode" | "structured_approvals">,
): PermissionMode | null {
  if (caps.plan_mode === "supported") return "plan";
  if (caps.structured_approvals === "supported") return "ask";
  return null;
}

/** Why an engine cannot review right now, or null when it can. */
export function reviewUnavailableReason(
  entry: HarnessDoctorEntry,
): string | null {
  // The server's own reason comes first: installing or signing in to the
  // engine would not change it. Grok CLI, which can't turn off network
  // access, is the one today.
  if (entry.review_blocked) return entry.review_blocked;
  if (!entry.found) return "Not installed";
  // A relay-covered or gateway-managed engine needs no local sign-in; only a
  // local one the probe saw signed out is refused, as the server refuses it.
  if (
    (entry.auth_mode ?? "local_sign_in") === "local_sign_in" &&
    entry.authenticated === false
  ) {
    return "Needs a sign-in";
  }
  if (!reviewPermissionMode(entry.caps)) return "Can't review read-only";
  return null;
}

export type ReviewEngineChoice = {
  readonly entry: HarnessDoctorEntry;
  /** Why it cannot review now, or null. */
  readonly reason: string | null;
};

/** Every engine a workspace can run, with why each cannot review, if so. */
export function reviewEngineChoices(
  doctor: readonly HarnessDoctorEntry[],
): ReviewEngineChoice[] {
  return workspaceHarnesses(doctor).map((entry) => ({
    entry,
    reason: reviewUnavailableReason(entry),
  }));
}

export type DefaultReviewEngine = {
  readonly kind: HarnessKind;
  /** The engine that wrote the changes, because no other one is ready. */
  readonly sameAsAuthor: boolean;
};

/**
 * The engine the form starts on: the one the person last reviewed with, if
 * it is ready and did not write the changes; else the first ready engine
 * that did not write them; else the author's own engine, if it is ready.
 * Null when no engine can review.
 */
export function defaultReviewEngine(
  choices: readonly ReviewEngineChoice[],
  author: HarnessKind | null,
  remembered: HarnessKind | null = null,
): DefaultReviewEngine | null {
  const ready = choices
    .filter((choice) => choice.reason === null)
    .map((choice) => choice.entry.kind);
  if (remembered && remembered !== author && ready.includes(remembered)) {
    return { kind: remembered, sameAsAuthor: false };
  }
  const other = ready.find((kind) => kind !== author);
  if (other) return { kind: other, sameAsAuthor: false };
  if (author && ready.includes(author)) {
    return { kind: author, sameAsAuthor: true };
  }
  return null;
}
