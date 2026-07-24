import type { ToolApprovalPreview } from "./api";

/**
 * Presentation of a tool's own preview of the action it is about to take.
 *
 * Renderer state holds no tool arguments; a preview is the narrow exception a
 * tool opts into so a human can see what they are approving. Formatting stays
 * here so the approval card and the tool card describe one action identically.
 */
export type ToolPreviewPresentation = {
  /** One-line form, used as a card title. */
  headline: string;
  /** Full form, one fact per line, used in a monospace block. */
  detail: string;
};

export function toolPreviewPresentation(
  preview: ToolApprovalPreview,
): ToolPreviewPresentation {
  const headline = [preview.command, ...preview.args]
    .map(quoteArgument)
    .join(" ");
  // The working directory is a fact about the command, not part of it, so it
  // reads as a comment rather than as something the shell would run. There is
  // no shell here at all — this is an argument vector.
  const detail =
    preview.cwd === "."
      ? headline
      : `${headline}\n# working directory: ${preview.cwd}`;
  return { headline, detail };
}

/**
 * Quote an argument only when leaving it bare would misrepresent where its
 * boundaries are. A vector element containing a space is one argument, and the
 * card must not read as though it were two.
 */
function quoteArgument(value: string): string {
  if (value.length === 0) return "''";
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(value)) return value;
  return `'${value.replaceAll("'", `'\\''`)}'`;
}
