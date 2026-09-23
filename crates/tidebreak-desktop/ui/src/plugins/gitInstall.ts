import { HttpError } from "@/api";

/** One archive member the importer left out. */
export type SkippedPluginMember = {
  path: string;
  reason: string;
};

/** Result of a successful `POST /plugins/install`. */
export type PluginInstallOutcome = {
  plugin: string;
  revision: string;
  skipped: SkippedPluginMember[];
};

export type GitInstallPhase =
  | { status: "empty" }
  | { status: "validating" }
  | { status: "installing" }
  | { status: "installed"; outcome: PluginInstallOutcome }
  | { status: "failed"; message: string };

export function validateGitInstallInput(
  url: string,
  revision: string,
): string | null {
  const trimmedUrl = url.trim();
  const trimmedRevision = revision.trim();
  if (!trimmedUrl) {
    return "Enter an https repository URL.";
  }
  let parsed: URL;
  try {
    parsed = new URL(trimmedUrl);
  } catch {
    return "Enter an https repository URL.";
  }
  if (parsed.protocol !== "https:") {
    return "Use an https repository URL.";
  }
  if (parsed.username || parsed.password) {
    return "The repository URL must not include credentials.";
  }
  if (!trimmedRevision) {
    return "Enter a tag or a full commit SHA.";
  }
  return null;
}

export function plainGitInstallError(error: unknown): string {
  if (error instanceof HttpError) {
    return mapKind(error.kind, error.message);
  }
  if (error instanceof Error) {
    return mapKind(undefined, error.message);
  }
  return "Could not install that plugin.";
}

function mapKind(kind: string | undefined, message: string): string {
  const lowered = `${kind ?? ""} ${message}`.toLowerCase();
  if (
    kind === "plugin_source_invalid" ||
    lowered.includes("plugin_source_invalid")
  ) {
    if (
      lowered.includes("revision") ||
      lowered.includes("tag or full commit")
    ) {
      return "Use a tag or a full commit SHA, not a branch.";
    }
    if (
      lowered.includes("https") ||
      lowered.includes("scheme") ||
      lowered.includes("url")
    ) {
      return "Use an https repository URL.";
    }
    return "That repository URL is not valid.";
  }
  if (
    kind === "plugin_source_unavailable" ||
    lowered.includes("plugin_source_unavailable")
  ) {
    if (lowered.includes("did not resolve") || lowered.includes("timed out")) {
      return "Could not reach that host.";
    }
    if (lowered.includes("404") || lowered.includes("not found")) {
      return "That tag or commit was not found.";
    }
    return "Could not fetch that repository.";
  }
  if (kind === "plugin_invalid" || lowered.includes("plugin_invalid")) {
    return "No recognizable plugin was found in that repository.";
  }
  if (kind === "plugin_conflict" || lowered.includes("plugin_conflict")) {
    return "A plugin with that name is already installed.";
  }
  if (kind === "plugin_install_unavailable") {
    return "Plugin install needs code execution.";
  }
  const trimmed = message.replace(/^Error:\s*/, "").trim();
  return trimmed && trimmed.length <= 240
    ? trimmed
    : "Could not install that plugin.";
}

export function parseInstallOutcome(value: unknown): PluginInstallOutcome {
  if (typeof value !== "object" || value === null) {
    throw new Error("Could not install that plugin.");
  }
  const record = value as Record<string, unknown>;
  if (
    typeof record.plugin !== "string" ||
    typeof record.revision !== "string"
  ) {
    throw new Error("Could not install that plugin.");
  }
  const skipped = Array.isArray(record.skipped)
    ? record.skipped.flatMap((entry) => {
        if (
          typeof entry !== "object" ||
          entry === null ||
          typeof (entry as { path?: unknown }).path !== "string" ||
          typeof (entry as { reason?: unknown }).reason !== "string"
        ) {
          return [];
        }
        const member = entry as { path: string; reason: string };
        return [{ path: member.path, reason: member.reason }];
      })
    : [];
  return {
    plugin: record.plugin,
    revision: record.revision,
    skipped,
  };
}
