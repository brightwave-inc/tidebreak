// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";

import type { CodeApprovalSnapshot } from "./api";
import { codeApprovalQuestion } from "./code/CodeApprovalCard";
import {
  MAX_QUESTION_CHARS,
  needsYouTitle,
  oneLine,
  waitingAnnouncement,
  workApprovalQuestion,
} from "./needsYou";
import { codeParkedQuestion } from "./needsYouQuestions";

function codeApproval(
  kind: CodeApprovalSnapshot["kind"],
  overrides: Partial<CodeApprovalSnapshot> = {},
): CodeApprovalSnapshot {
  return {
    id: "a1",
    session_id: "s1",
    turn_id: "t1",
    kind,
    harness_raw_json: "{}",
    state: "pending",
    requested_at: "2026-09-23T10:00:00.000Z",
    ...overrides,
  };
}

describe("needs-you wording", () => {
  it("titles a notice by what the agent needs", () => {
    expect(needsYouTitle("approval", "Fix login")).toBe(
      "Needs your approval: Fix login",
    );
    expect(needsYouTitle("question", "Fix login")).toBe(
      "Needs your answer: Fix login",
    );
    expect(needsYouTitle("plan", "Fix login")).toBe(
      "Needs your review: Fix login",
    );
  });

  it("announces the wait even when the question could not be read", () => {
    expect(waitingAnnouncement({ kind: "approval", text: "" })).toBe(
      "Waiting for your approval",
    );
  });

  it("reads one line and cuts it to fit a banner", () => {
    expect(oneLine("\n\n  Which   week?\nSecond line")).toBe("Which week?");
    const long = oneLine("word ".repeat(60));
    expect(Array.from(long)).toHaveLength(MAX_QUESTION_CHARS);
    expect(long.endsWith("…")).toBe(true);
  });

  it("keeps a direction override from reordering what a banner says", () => {
    // A right-to-left override reverses the text after it, so a banner could
    // show something other than what the agent asked. Built from code points
    // so this file carries no invisible characters of its own.
    const override = String.fromCodePoint(0x202e);
    const popDirection = String.fromCodePoint(0x202c);
    const rightToLeftMark = String.fromCodePoint(0x200f);
    const bell = String.fromCodePoint(0x07);
    expect(oneLine(`Delete ${override}eliflog${popDirection} the logs`)).toBe(
      "Delete eliflog the logs",
    );
    expect(
      needsYouTitle("approval", `Fix${bell} login${rightToLeftMark}`),
    ).toBe("Needs your approval: Fix login");
  });

  it("names the action after a short ask, and lets a long ask stand alone", () => {
    expect(
      workApprovalQuestion("Run a command", {
        tool: "exec",
        command: "npm",
        args: ["test"],
        cwd: ".",
        files: [],
      }).text,
    ).toBe("Run this command? npm test");
    const ask =
      "Allow web search to send this query and its explicit filters to the configured search provider outside Tidebreak?";
    expect(
      workApprovalQuestion(ask, {
        tool: "web_search",
        query: "tidebreak release notes",
        domains: [],
        start_published_at: null,
        end_published_at: null,
      }).text,
    ).toBe(oneLine(ask));
  });
});

describe("a parked Code approval as a question", () => {
  it("asks what each card asks", () => {
    expect(
      codeApprovalQuestion(
        codeApproval({ type: "command", cmd: "cargo test\n--nocapture" }),
      ),
    ).toEqual({ kind: "approval", text: "Run this command? cargo test" });
    expect(
      codeApprovalQuestion(
        codeApproval({ type: "file_write", paths: ["a.ts", "b.ts", "c.ts"] }),
      ),
    ).toEqual({ kind: "approval", text: "Write these 3 files?" });
    expect(
      codeApprovalQuestion(
        codeApproval(
          { type: "plan", proposed_mode: "ask" },
          {
            harness_raw_json: JSON.stringify({
              title: "Split the billing client",
              plan: "1. Read it",
            }),
          },
        ),
      ),
    ).toEqual({ kind: "plan", text: "Split the billing client" });
  });

  it("reads the newest pending approval for a notice", async () => {
    const listCodeApprovals = vi
      .fn()
      .mockResolvedValue([
        codeApproval(
          { type: "command", cmd: "ls" },
          { id: "old", requested_at: "2026-09-23T09:00:00.000Z" },
        ),
        codeApproval(
          { type: "command", cmd: "rm -rf build" },
          { id: "new", requested_at: "2026-09-23T10:00:00.000Z" },
        ),
      ]);

    await expect(
      codeParkedQuestion({ listCodeApprovals }, "s1"),
    ).resolves.toEqual({
      kind: "approval",
      text: "Run this command? rm -rf build",
    });
    expect(listCodeApprovals).toHaveBeenCalledWith({
      state: "pending",
      sessionId: "s1",
    });
  });
});
