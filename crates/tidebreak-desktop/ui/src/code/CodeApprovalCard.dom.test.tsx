// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeApprovalCard } from "./CodeApprovalCard";
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

describe("CodeApprovalCard", () => {
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
