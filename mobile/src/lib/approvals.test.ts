import { describe, expect, it } from "vitest";
import type { CodeApprovalSnapshot } from "../generated/wire";
import {
  approvalSummary,
  approvalTitle,
  formatApprovalPayload,
  pendingApprovals,
  validDenialFeedback,
} from "./approvals";

function approval(
  id: string,
  state: CodeApprovalSnapshot["state"],
  requestedAt: string,
): CodeApprovalSnapshot {
  return {
    id,
    session_id: "session-1",
    turn_id: "turn-1",
    kind: { type: "command", cmd: "cargo test", cwd: "/workspace" },
    harness_raw_json: "{}",
    state,
    requested_at: requestedAt,
  };
}

describe("code approval presentation", () => {
  it("filters settled rows and orders the oldest decision first", () => {
    expect(
      pendingApprovals([
        approval("new", "pending", "2026-08-27T00:00:02Z"),
        approval("done", "approved", "2026-08-27T00:00:00Z"),
        approval("old", "pending", "2026-08-27T00:00:01Z"),
      ]).map((item) => item.id),
    ).toEqual(["old", "new"]);
  });

  it("requires meaningful denial feedback", () => {
    expect(validDenialFeedback("   ")).toBeNull();
    expect(validDenialFeedback("  Use the focused test. ")).toBe(
      "Use the focused test.",
    );
  });

  it("names and summarizes every structured approval kind", () => {
    expect(approvalTitle({ type: "command", cmd: "ls" })).toBe("Run command");
    expect(approvalSummary({ type: "command", cmd: "", cwd: "/tmp" })).toBe(
      "Command\ncwd /tmp",
    );
    expect(
      approvalSummary({ type: "file_write", paths: ["src/a.ts", "src/b.ts"] }),
    ).toBe("src/a.ts\nsrc/b.ts");
    expect(approvalSummary({ type: "network", summary: "github.com" })).toBe(
      "github.com",
    );
  });

  it("pretty-prints the complete server-capped harness payload", () => {
    expect(formatApprovalPayload('{"cmd":"cargo test"}')).toBe(
      '{\n  "cmd": "cargo test"\n}',
    );
    expect(formatApprovalPayload("not-json")).toBe("not-json");
    const cappedByServer = JSON.stringify({ preview: "x".repeat(5_000) });
    expect(formatApprovalPayload(cappedByServer)).toBe(
      JSON.stringify(JSON.parse(cappedByServer), null, 2),
    );
  });
});
