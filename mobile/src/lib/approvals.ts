import type {
  CodeApprovalKind,
  CodeApprovalSnapshot,
  NetworkPolicy,
  ToolActionPreview,
} from "../generated/wire";

export function approvalTitle(kind: CodeApprovalKind): string {
  switch (kind.type) {
    case "command":
      return "Run command";
    case "file_write":
      return "Write files";
    case "network":
      return "Use network";
    case "other":
      return "Allow action";
    case "tool_use":
      return "Run tool";
    case "questions":
      return "Answer questions";
    case "plan":
      return "Review plan";
  }
}

export function approvalSummary(kind: CodeApprovalKind): string {
  switch (kind.type) {
    case "command": {
      const command = kind.cmd || "Command";
      return kind.cwd ? `${command}\ncwd ${kind.cwd}` : command;
    }
    case "file_write":
      return kind.paths.join("\n") || "The engine requested a file write.";
    case "network":
    case "other":
      return kind.summary.trim() || "The engine requested approval.";
    case "tool_use":
      // Decision 0018: consent is given to the literal action, never to the
      // call's own display-only narration.
      return toolActionDetail(kind.preview);
    case "questions":
      return kind.questions.map((question) => question.question).join("\n");
    case "plan":
      return `The engine proposed a plan. Accepting moves the session to ${kind.proposed_mode}.`;
  }
}


/**
 * The literal action a tool_use approval asks consent for, one fact per
 * line. Mirrors the desktop's `toolPreviewPresentation().detail`; the
 * preview's `summary` is display-only and never rendered here.
 */
function toolActionDetail(preview: ToolActionPreview): string {
  switch (preview.tool) {
    case "search":
      return `${preview.query}\n# searched against this conversation's sources`;
    case "web_search":
      return [
        preview.query,
        preview.domains.length > 0
          ? `# limited to ${preview.domains.join(", ")}`
          : null,
        "# sent to the configured web search provider",
      ]
        .filter((line): line is string => line !== null)
        .join("\n");
    case "web_extract":
      return `${preview.url}\n# fetched from the public web`;
    case "write_file":
      return `${preview.path}\n# written into this work's workspace`;
    case "delegate_agent":
      return [
        preview.task,
        `# network: ${networkLabel(preview.network)}`,
      ].join("\n");
    case "exec":
      return [
        [preview.command, ...preview.args].join(" "),
        preview.cwd !== "." ? `# working directory: ${preview.cwd}` : null,
        preview.files.length > 0
          ? `# staged files: ${preview.files.join(", ")}`
          : null,
      ]
        .filter((line): line is string => line !== null)
        .join("\n");
  }
}

function networkLabel(policy: NetworkPolicy): string {
  switch (policy.mode) {
    case "off":
      return "no network";
    case "package_managers":
      return "package managers only";
    case "allowed_hosts":
      return policy.allowed_hosts.join(", ") || "listed hosts only";
    case "open":
      return "open";
  }
}

export function pendingApprovals(
  approvals: readonly CodeApprovalSnapshot[],
): CodeApprovalSnapshot[] {
  return approvals
    .filter((approval) => approval.state === "pending")
    .sort((left, right) => left.requested_at.localeCompare(right.requested_at));
}

export function validDenialFeedback(value: string): string | null {
  const feedback = value.trim();
  return feedback.length > 0 ? feedback : null;
}

export function formatApprovalPayload(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2) ?? raw;
  } catch {
    // Show non-JSON payloads exactly as the harness supplied them.
    return raw;
  }
}
