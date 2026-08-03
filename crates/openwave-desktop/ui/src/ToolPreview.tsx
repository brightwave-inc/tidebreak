import type { ExecResultPreview, ToolActionPreview } from "./api";

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
  result: ExecResultPreview | null = null,
): ToolPreviewPresentation {
  if (preview.tool === "search") {
    const headline = preview.query;
    return {
      headline,
      detail: `${headline}\n# searched against this conversation's sources`,
    };
  }
  if (preview.tool === "web_search") {
    // The query leads because it is the action, but the filters go to the
    // provider with it. Leaving them off described part of the thing the card
    // was asking about.
    const headline = preview.query;
    const detail = [
      headline,
      preview.domains.length > 0 && `# limited to ${preview.domains.join(", ")}`,
      publishedWindow(preview),
      "# sent to the configured web search provider",
    ]
      .filter((line): line is string => typeof line === "string")
      .join("\n");
    return { headline, detail };
  }
  if (preview.tool === "write_file") {
    // The path is the resource under review: the card says where the write
    // lands, and the content deliberately never crosses the boundary.
    const headline = preview.path;
    return {
      headline,
      detail: `${headline}\n# written into this chat's workspace`,
    };
  }
  if (preview.tool === "web_extract") {
    // The URL is the whole action: what leaves the device and where the
    // request goes are the same string, so the card shows it unabridged.
    const headline = preview.url;
    return {
      headline,
      detail: `${headline}\n# fetched from the public web`,
    };
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
 * The publication window a web search will accept, or nothing when it is open
 * at both ends. Dates are shown as the model wrote them, because what the card
 * is for is showing what the provider is told.
 */
function publishedWindow(
  preview: Extract<ToolActionPreview, { tool: "web_search" }>,
): string | null {
  const from = preview.start_published_at;
  const to = preview.end_published_at;
  if (from && to) return `# published between ${from} and ${to}`;
  if (from) return `# published on or after ${from}`;
  if (to) return `# published on or before ${to}`;
  return null;
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
