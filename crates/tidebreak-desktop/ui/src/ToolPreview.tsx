import type {
  ExecResultPreview,
  NetworkPolicy,
  ToolActionPreview,
} from "./api";
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
      preview.domains.length > 0 &&
        `# limited to ${preview.domains.join(", ")}`,
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
    result &&
      !result.timedOut &&
      result.exitCode === null &&
      "# killed by a signal",
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
 * When the model left no sentence, the headline is still grounded in the
 * action: a short verb plus the most meaningful target (path, URL, query, or
 * command head) so a long argv cannot become the whole transcript width.
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
  if (preview.tool === "exec") {
    return {
      text: groundedExecHeadline(preview.command, preview.args),
      literal: true,
    };
  }
  return { text: toolPreviewPresentation(preview).headline, literal: true };
}

/**
 * A short factual line drawn from the command's own streams — never a second
 * model call. Prefer stderr for failures, a single quiet stdout line for a
 * clean finish, and nothing when the streams are noise.
 */
export function toolResultFact(
  result: ExecResultPreview | null,
  options: { failed?: boolean } = {},
): string | null {
  if (!result) return null;
  if (result.timedOut) return "Timed out";
  if (result.exitCode === null) return "Stopped by a signal";

  const failed =
    options.failed === true ||
    (typeof result.exitCode === "number" && result.exitCode !== 0);

  if (failed) {
    const fromStderr = firstMeaningfulLine(result.stderr);
    if (fromStderr) return clipFact(fromStderr);
    const fromStdout = firstMeaningfulLine(result.stdout);
    if (fromStdout) return clipFact(fromStdout);
    if (typeof result.exitCode === "number") {
      return `Exit ${result.exitCode}`;
    }
    return null;
  }

  // A successful run stays quiet unless stdout is already one short fact.
  const line = firstMeaningfulLine(result.stdout);
  if (!line) return null;
  if (result.stdout.replace(/\n+$/, "").includes("\n")) return null;
  if (line.length > FACT_MAX) return null;
  return line;
}

/** Cap on a collapsed-row fact so it cannot become a second transcript. */
const FACT_MAX = 120;

/**
 * Grounded exec title when the model left no summary: command name plus the
 * most informative argument (usually a path or script), not the full argv.
 */
export function groundedExecHeadline(
  command: string,
  args: readonly string[],
): string {
  const base = commandBaseName(command);
  const target = meaningfulExecTarget(args);
  if (!target) return base;
  return `${base} ${target}`;
}

function meaningfulExecTarget(args: readonly string[]): string | null {
  // Walk from the end: the last path-like token is usually the file or package
  // under review; flags and short options stay out of the collapsed title.
  for (let index = args.length - 1; index >= 0; index -= 1) {
    const arg = args[index];
    if (!arg || arg.startsWith("-")) continue;
    if (arg.includes("/") || arg.includes("\\") || /\.\w{1,8}$/.test(arg)) {
      return commandBaseName(arg);
    }
  }
  for (let index = args.length - 1; index >= 0; index -= 1) {
    const arg = args[index];
    if (!arg || arg.startsWith("-")) continue;
    return arg.length > 48 ? `${arg.slice(0, 45)}…` : arg;
  }
  return null;
}

function commandBaseName(value: string): string {
  const trimmed = value.replace(/[\\/]+$/, "");
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] || trimmed || value;
}

function firstMeaningfulLine(text: string): string | null {
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    // Stream section headers and shell prompts are structure, not a fact.
    if (line === "$ stdout" || line === "$ stderr") continue;
    if (line.startsWith("$ ")) continue;
    if (line.startsWith("# ")) continue;
    return line;
  }
  return null;
}

function clipFact(line: string): string {
  if (line.length <= FACT_MAX) return line;
  return `${line.slice(0, FACT_MAX - 1)}…`;
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
