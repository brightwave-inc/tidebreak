import type { SequencedEventFrame as SequencedCodeEventFrame } from "../generated/wire";

/**
 * Resume cursor for `GET /sessions/{id}/events?after=`.
 *
 * Transient frames stamp the journal position they streamed behind; applying
 * them must not advance the cursor. Resume from the last durable seq.
 */
export function resumeAfter(
  lastSeq: number,
  frame: Pick<SequencedCodeEventFrame, "seq" | "transient">,
): number {
  if (frame.transient === true) {
    return lastSeq;
  }
  return Math.max(lastSeq, frame.seq);
}

export function shouldApplyDurable(
  lastSeq: number,
  frame: Pick<SequencedCodeEventFrame, "seq" | "transient">,
): boolean {
  if (frame.transient === true) {
    return true;
  }
  return frame.seq > lastSeq;
}
