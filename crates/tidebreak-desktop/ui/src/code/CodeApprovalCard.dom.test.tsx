// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeApprovalCard, MAX_PAYLOAD_CHARS } from "./CodeApprovalCard";
import type { CodeApprovalSnapshot } from "../api/types";

afterEach(() => {
  cleanup();
});

const pendingCommand: CodeApprovalSnapshot = {
  id: "appr-1",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: { type: "command", cmd: "rm -rf /tmp/scratch", cwd: "/workspace" },
  harness_raw_json: JSON.stringify({
    tool_name: "Bash",
    command: "rm -rf /tmp/scratch",
    tool_use_id: "toolu_1",
  }),
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

const pendingWrite: CodeApprovalSnapshot = {
  id: "appr-2",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: { type: "file_write", paths: ["/workspace/probe.txt"] },
  harness_raw_json: JSON.stringify({
    tool_name: "Write",
    input: { file_path: "/workspace/probe.txt", content: "hello" },
    tool_use_id: "toolu_2",
  }),
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

const pendingToolUse: CodeApprovalSnapshot = {
  id: "appr-3",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: {
    type: "tool_use",
    preview: {
      tool: "exec",
      command: "rm",
      args: ["-rf", "two words"],
      cwd: "work",
      files: ["notes.md"],
      summary: "Cleaning temporary caches",
    },
    offered_grants: [],
  },
  harness_raw_json: "",
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

const pendingQuestions: CodeApprovalSnapshot = {
  id: "appr-4",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: {
    type: "questions",
    questions: [
      {
        id: "q1",
        header: "Region",
        question: "Which region should the deploy target?",
        options: [
          { id: "east", label: "us-east", description: "" },
          { id: "west", label: "us-west", description: "" },
        ],
        question_type: "single_select",
        allow_free_form: false,
      },
    ],
  },
  harness_raw_json: "",
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

const pendingPlan: CodeApprovalSnapshot = {
  id: "appr-5",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: { type: "plan", proposed_mode: "auto" },
  harness_raw_json: "",
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

describe("CodeApprovalCard", () => {
  it("shows the literal tool action, never the model's narration", () => {
    render(<CodeApprovalCard approval={pendingToolUse} onDecide={vi.fn()} />);
    expect(screen.getByText("Run this tool?")).toBeInTheDocument();
    const detail = screen.getByText(/rm -rf 'two words'/);
    expect(detail.tagName).toBe("PRE");
    expect(detail.textContent).toContain("# working directory: work");
    expect(detail.textContent).toContain("# staged files: notes.md");
    // Decision 0018: the display-only summary never reaches the consent card.
    expect(
      screen.queryByText(/Cleaning temporary caches/),
    ).not.toBeInTheDocument();
  });

  it("lists the questions and options the engine is asking", () => {
    render(<CodeApprovalCard approval={pendingQuestions} onDecide={vi.fn()} />);
    expect(screen.getByText("Answer these questions?")).toBeInTheDocument();
    expect(
      screen.getByText("Which region should the deploy target?"),
    ).toBeInTheDocument();
    expect(screen.getByText("us-east · us-west")).toBeInTheDocument();
  });

  it("names the mode a plan approval would move the session to", () => {
    render(<CodeApprovalCard approval={pendingPlan} onDecide={vi.fn()} />);
    expect(screen.getByText("Approve this plan?")).toBeInTheDocument();
    expect(screen.getByText("auto")).toBeInTheDocument();
  });

  it("leads with the command and keeps the harness payload collapsed", () => {
    render(<CodeApprovalCard approval={pendingCommand} onDecide={vi.fn()} />);
    expect(screen.getByText("Run this command?")).toBeInTheDocument();
    expect(screen.getByText("rm -rf /tmp/scratch").tagName).toBe("PRE");
    expect(screen.getByText("cwd /workspace")).toBeInTheDocument();
    expect(
      document.querySelector('time[datetime="2026-08-15T12:00:00.000Z"]'),
    ).not.toBeNull();

    const toggle = screen.getByRole("button", { name: "Harness payload" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText(/"tool_name": "Bash"/)).toBeInTheDocument();
  });

  it("lists the paths a file write would touch", () => {
    render(<CodeApprovalCard approval={pendingWrite} onDecide={vi.fn()} />);
    expect(screen.getByText("Write this file?")).toBeInTheDocument();
    expect(screen.getByText("/workspace/probe.txt")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Harness payload" }),
    ).toHaveAttribute("aria-expanded", "false");
  });

  it("opens a feedback field on deny and submits it", () => {
    const onDecide = vi.fn();
    render(<CodeApprovalCard approval={pendingCommand} onDecide={onDecide} />);
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    const box = screen.getByRole("textbox", { name: "Denial feedback" });
    fireEvent.change(box, {
      target: { value: "no — use the fixtures directory instead" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(onDecide).toHaveBeenCalledWith(
      "deny",
      "no — use the fixtures directory instead",
    );
  });

  it("caps a payload an engine sent a whole file in", () => {
    const huge = "x".repeat(MAX_PAYLOAD_CHARS * 2);
    render(
      <CodeApprovalCard
        approval={{
          ...pendingWrite,
          harness_raw_json: JSON.stringify({ content: huge }),
        }}
        onDecide={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Harness payload" }));
    const shown = screen.getByText(/more characters not shown/);
    expect(shown.textContent!.length).toBeLessThan(MAX_PAYLOAD_CHARS + 200);
  });

  it("tones an approved card as success and stamps both times", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingCommand,
          state: "approved",
          decided_at: "2026-08-15T12:05:00.000Z",
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Approved")).toHaveClass("text-success-foreground");
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
    expect(
      document.querySelector('time[datetime="2026-08-15T12:00:00.000Z"]'),
    ).not.toBeNull();
    expect(
      document.querySelector('time[datetime="2026-08-15T12:05:00.000Z"]'),
    ).not.toBeNull();
  });

  it("drops the buttons on an approval nobody decided", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingCommand,
          state: "abandoned",
          decided_at: "2026-08-15T12:01:00.000Z",
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Not decided")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Deny" })).toBeNull();
    expect(
      screen.getByText(/a decision can no longer\s+reach it/),
    ).toBeInTheDocument();
  });

  it("does not render an unknown engine summary as the decision", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingCommand,
          kind: { type: "other", summary: "unknown" },
          harness_raw_json: "null",
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Allow this?")).toBeInTheDocument();
    expect(screen.getByText("The engine needs approval")).toBeInTheDocument();
    expect(screen.queryByText("unknown")).not.toBeInTheDocument();
  });

  it("tones a denial as a warning and shows the feedback", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingCommand,
          state: "denied",
          feedback: "no — use the fixtures directory instead",
          decided_at: "2026-08-15T12:05:00.000Z",
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Denied")).toHaveClass("text-warning-foreground");
    expect(
      screen.getByText("no — use the fixtures directory instead"),
    ).toBeInTheDocument();
  });
});
