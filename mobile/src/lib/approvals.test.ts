import { describe, expect, it } from "vitest";
import type { ApprovalSnapshot as CodeApprovalSnapshot } from "../generated/wire";
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

  it("shows the literal action for tool_use, never the model's narration", () => {
    // Decision 0018: the display-only summary must not reach the consent card.
    expect(
      approvalSummary({
        type: "tool_use",
        preview: {
          tool: "exec",
          command: "rm",
          args: ["-rf", "cache"],
          cwd: "work",
          files: ["notes.md"],
          summary: "Cleaning temporary caches",
        },
        offered_grants: [],
      }),
    ).toBe("rm -rf cache\n# working directory: work\n# staged files: notes.md");
    expect(
      approvalSummary({
        type: "tool_use",
        preview: {
          tool: "web_extract",
          url: "https://example.com/report",
          summary: "Reading the report",
        },
        offered_grants: [],
      }),
    ).toBe("https://example.com/report\n# fetched from the public web");
    // Argument boundaries survive: one argument with a space is not two.
    expect(
      approvalSummary({
        type: "tool_use",
        preview: {
          tool: "exec",
          command: "git",
          args: ["commit", "-m", "fix: two words"],
          cwd: ".",
          files: [],
        },
        offered_grants: [],
      }),
    ).toBe("git commit -m 'fix: two words'");
    // The provider-bound filters are part of the action being consented to.
    expect(
      approvalSummary({
        type: "tool_use",
        preview: {
          tool: "web_search",
          query: "tidebreak",
          domains: [],
          start_published_at: "2026-01-01",
          end_published_at: null,
        },
        offered_grants: [],
      }),
    ).toBe(
      "tidebreak\n# published on or after 2026-01-01\n# sent to the configured web search provider",
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
