import type { ToolActionPreview, ToolResultPreview } from "./api";

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
  preview: ToolActionPreview,
  result: ToolResultPreview | null = null,
): ToolPreviewPresentation {
  if (preview.tool === "search" || preview.tool === "web_search") {
    // The query is the whole action. For a web search it is also the thing
    // that leaves the device, which is what the reader is being asked about.
    const headline = preview.query;
    const detail =
      preview.tool === "web_search"
        ? `${headline}\n# sent to the configured web search provider`
        : `${headline}\n# searched against this conversation's sources`;
    return { headline, detail };
  }
  const headline = [preview.command, ...preview.args]
    .map(quoteArgument)
    .join(" ");
  // Everything below the command is a fact *about* it, so it reads as a
  // comment rather than as something a shell would run. There is no shell here
  // at all — this is an argument vector.
  const detail = [
    headline,
    preview.cwd !== "." && `# working directory: ${preview.cwd}`,
    result?.timedOut && "# stopped at the time limit",
    result && !result.timedOut && result.exitCode === null && "# killed by a signal",
    result?.exitCode !== null &&
      result?.exitCode !== undefined &&
      `# exit code: ${result.exitCode}`,
  ]
    .filter((line): line is string => typeof line === "string")
    .join("\n");
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
