import type { ExecResultPreview, NetworkPolicy, ToolActionPreview } from "./api";
import { networkPolicyLabel } from "./NetworkPolicyDialog";

/**
 * Presentation of a tool's own preview of the action it is about to take.
 *
 * Renderer state holds no tool arguments; a preview is the narrow exception a
 * tool opts into so a human can see what they are approving. Formatting stays
 * here so the approval card and the tool card describe one action identically.
 *
 * Deliberately literal, both fields: the action as the tool stated it, never
 * the call's own `summary`. The approval card renders `detail`, and consent is
 * given to a command rather than to a sentence about one — see
 * `docs/decisions/0018-tool-call-narration.md`. Prose belongs to
 * {@link toolPreviewHeadline}, which only result cards call.
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
      detail: `${headline}\n# written into this work's workspace`,
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
  if (preview.tool === "delegate_agent") {
    // The task leads because it is what the run will do, but the network
    // policy is the part being consented to: the run's workspace is its own,
    // so what it can reach is the only way anything leaves the box.
    const headline = preview.task;
    const detail = [
      headline,
      `# network: ${networkPolicyLabel(preview.network)}${networkHosts(preview.network)}`,
      "# runs unattended in its own workspace; its own calls are not asked about",
    ].join("\n");
    return { headline, detail };
  }
  const headline = execCommandHeadline(preview.command, preview.args);
  // Everything below the command is a fact *about* it, so it reads as a
  // comment rather than as something a shell would run. There is no shell here
  // at all — this is an argument vector.
  const detail = [
    headline,
    preview.cwd !== "." && `# working directory: ${preview.cwd}`,
    // What the command is handed is part of what it will do, so the card that
    // asks for consent says which files it can read.
    preview.files.length > 0 && `# staged files: ${preview.files.join(", ")}`,
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
 * The one line a settled call is worth reading, and whether it is prose.
 *
 * A card's collapsed state is the only thing most readers ever see, and an
 * argument vector is not readable to someone who does not read shell — so the
 * model is asked to say what it is doing, and that sentence leads. The literal
 * action is never lost: it is one click away in the card's own body, and it is
 * still the only thing an approval card shows.
 *
 * `literal` is what tells a caller to set a monospace face: prose in monospace
 * reads as something a shell would run, which it is not.
 */
export function toolPreviewHeadline(preview: ToolActionPreview): {
  text: string;
  literal: boolean;
} {
  const summary = "summary" in preview ? preview.summary : undefined;
  if (typeof summary === "string" && summary.length > 0) {
    return { text: summary, literal: false };
  }
  return { text: toolPreviewPresentation(preview).headline, literal: true };
}

/**
 * The one-line form of a command and its argument vector.
 *
 * Shared so a command reads identically wherever it is shown: the foreground
 * approval and result cards, and the background run's activity timeline, where
 * it is the card's headline unless the step narrated itself, and its body
 * either way.
 */
export function execCommandHeadline(
  command: string,
  args: readonly string[],
): string {
  return [command, ...args].map(quoteArgument).join(" ");
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
 * The hosts a custom policy names, so the card says which destinations the
 * delegated run can reach rather than only that the list exists.
 */
function networkHosts(policy: NetworkPolicy): string {
  if (policy.mode !== "allowed_hosts") return "";
  const hosts = policy.allowed_hosts.join(", ");
  return hosts.length > 0 ? ` (${hosts})` : "";
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
