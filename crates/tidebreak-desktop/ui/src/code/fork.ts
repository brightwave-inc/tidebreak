import type { ComposerWorkspaceFile } from "@/Composer";
import type { CodeForkTranscript } from "../api/types";

/**
 * Forking one agent into another: what the child is handed, and how.
 *
 * The server writes the fork into private storage as one directory — the
 * condensed transcript plus a full record per turn — so the child needs an
 * absolute path rather than bytes. The transcript path travels as a composer
 * chip and is named on the way out; the framing lines tell the child how to
 * use the two layers, and stay the reader's to edit.
 */

/** The opening lines a fork seeds, editable before they are sent. */
export function forkFraming(transcript: CodeForkTranscript): string {
  const base =
    "Read the attached transcript first, then pick up where that agent left off. " +
    "It is condensed: each turn's full record — complete tool output and " +
    "subagent activity — sits in the same directory, so open one only when " +
    "the transcript is not enough.";
  if (transcript.at_turn_ordinal === undefined) return base;
  return (
    `${base} The transcript ends at turn ${transcript.at_turn_ordinal}, ` +
    "where this fork was taken; the conversation continued past it, so the " +
    "worktree may hold changes the transcript does not describe."
  );
}

/** The transcript chip for a fork the server has already written. */
export function forkTranscriptFile(
  transcript: CodeForkTranscript,
): ComposerWorkspaceFile {
  const turns = `${transcript.turns} ${transcript.turns === 1 ? "turn" : "turns"}`;
  return {
    path: transcript.path,
    detail: transcript.truncated
      ? `Transcript, most recent ${turns}`
      : `Transcript, ${turns}`,
  };
}

/**
 * Name the files a message points at, after the message itself.
 *
 * The engine reads them from disk, so the prompt carries paths and nothing
 * else. An empty list leaves the message exactly as the reader wrote it.
 */
export function messageWithWorkspaceFiles(
  message: string,
  files: readonly ComposerWorkspaceFile[],
): string {
  if (files.length === 0) return message;
  const list = files.map((file) => `- \`${file.path}\``).join("\n");
  const body = `Files available to you:\n${list}`;
  const text = message.trim();
  return text.length === 0 ? body : `${text}\n\n${body}`;
}
