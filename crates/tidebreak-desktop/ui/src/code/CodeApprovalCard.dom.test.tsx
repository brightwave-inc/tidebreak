// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeApprovalCard } from "./CodeApprovalCard";
import type { CodeApprovalSnapshot } from "../api/types";

afterEach(() => {
  cleanup();
});

const pending: CodeApprovalSnapshot = {
  id: "appr-1",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: { type: "file_write", paths: ["/workspace/probe.txt"] },
  harness_raw_json: JSON.stringify({
    tool_name: "Write",
    input: { file_path: "/workspace/probe.txt", content: "hello" },
    tool_use_id: "toolu_1",
  }),
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

describe("CodeApprovalCard", () => {
  it("renders the harness payload rather than a paraphrase", () => {
    render(<CodeApprovalCard approval={pending} onDecide={vi.fn()} />);
    expect(screen.getByText("Write this file?")).toBeInTheDocument();
    expect(screen.getByText(/"tool_name": "Write"/)).toBeInTheDocument();
    expect(screen.getByText(/"file_path": "\/workspace\/probe.txt"/)).toBeInTheDocument();
  });

  it("opens a feedback field on deny and submits it", () => {
    const onDecide = vi.fn();
    render(<CodeApprovalCard approval={pending} onDecide={onDecide} />);
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
});
