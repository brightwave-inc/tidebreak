import type { CodeApprovalKind, CodeApprovalSnapshot } from "../generated/wire";

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
    case "tool_use": {
      const preview = kind.preview;
      const summary =
        "summary" in preview && typeof preview.summary === "string"
          ? preview.summary.trim()
          : "";
      return summary || "The engine requested a tool call.";
    }
    case "questions":
      return kind.questions.map((question) => question.question).join("\n");
    case "plan":
      return `The engine proposed a plan. Accepting moves the session to ${kind.proposed_mode}.`;
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
