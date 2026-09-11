import type {
  Attention,
  CodeSessionDigest,
  CodeSessionSnapshot,
  FenceReason,
} from "../api/types";
import { fenceReasonText } from "./labels";

export function automaticRecoveryReason(
  reason: FenceReason | undefined,
): boolean {
  return (
    reason?.type === "orphan_alive" ||
    reason?.type === "resume_lost" ||
    reason?.type === "sandbox_lost"
  );
}

/** Recovery status stays visible even when the reader has pinned the session. */
export function recoveryAttention(
  lifecycle: CodeSessionSnapshot["lifecycle"] | undefined,
  attention: Attention | undefined,
  reason: FenceReason | undefined,
): Attention | undefined {
  if (lifecycle !== "fenced" || attention?.state.type === "needs_you")
    return attention;
  if (!reason) return attention;
  if (automaticRecoveryReason(reason))
    return { state: { type: "fenced", reason }, source: "lifecycle" };
  return {
    state: {
      type: "needs_you",
      prompt: fenceReasonText(reason),
      source: "lifecycle",
    },
    source: "lifecycle",
  };
}

/** The live digest supersedes the snapshot, including a stale recovery reason. */
export function sessionRecoveryState(
  session: CodeSessionSnapshot | null | undefined,
  digest: CodeSessionDigest | undefined,
) {
  const lifecycle = digest?.lifecycle ?? session?.lifecycle;
  const rawAttention = digest?.attention ?? session?.attention;
  const reason = digest
    ? (digest.fence_reason ??
      (digest.attention.state.type === "fenced"
        ? digest.attention.state.reason
        : undefined))
    : session?.fence_reason;
  return {
    lifecycle,
    attention:
      digest &&
      lifecycle === "fenced" &&
      !digest.fence_reason &&
      rawAttention?.state.type !== "needs_you"
        ? ({
            state: {
              type: "needs_you",
              prompt:
                "The engine connection stopped. Retry recovery to continue with the saved transcript.",
              source: "lifecycle",
            },
            source: "lifecycle",
          } as Attention)
        : recoveryAttention(lifecycle, rawAttention, reason),
    reason,
    blocksTurn: lifecycle === "fenced",
  };
}

export function recoveryDigest(digest: CodeSessionDigest): CodeSessionDigest {
  const state = sessionRecoveryState(undefined, digest);
  return state.attention && state.attention !== digest.attention
    ? { ...digest, attention: state.attention }
    : digest;
}
