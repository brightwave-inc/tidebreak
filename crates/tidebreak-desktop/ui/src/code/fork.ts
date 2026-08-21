import type { ComposerWorkspaceFile } from "@/Composer";
import type { CodeForkTranscript } from "../api/types";

/**
 * Forking one agent into another: what the child is handed, and how.
 *
 * The server writes the parent's transcript into the worktree, so the child
 * needs a path rather than bytes. That path travels as a composer chip and is
 * named on the way out, which keeps the framing line the reader's to edit.
 */

/** The opening line a fork seeds, editable before it is sent. */
export const FORK_FRAMING =
  "Read the attached transcript first, then pick up where that agent left off.";

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
 * Name the worktree files a message points at, after the message itself.
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
  const body = `Files in this worktree:\n${list}`;
  const text = message.trim();
  return text.length === 0 ? body : `${text}\n\n${body}`;
}
