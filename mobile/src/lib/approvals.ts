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
