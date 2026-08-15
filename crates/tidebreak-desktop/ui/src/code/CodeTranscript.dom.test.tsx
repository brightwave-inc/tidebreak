// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { CodeTranscript } from "./CodeTranscript";
import type { CodeTranscriptItem } from "./CodeSessionReducer";

afterEach(() => {
  cleanup();
});

const items: CodeTranscriptItem[] = [
  { kind: "user", id: "u1", turnId: "t1", text: "list the files" },
  {
    kind: "assistant",
    id: "a1",
    turnId: "t1",
    text: "Looking at the tree.",
    streaming: false,
  },
  {
    kind: "tool",
    id: "tool1",
    turnId: "t1",
    callId: "c1",
    name: "Bash",
    detail: { kind: "command", cmd: "ls", cwd: "/tmp" },
    status: "succeeded",
    preview: "README.md",
  },
  {
    kind: "notice",
    id: "n1",
    level: "warning",
    message: "unrecognized event dropped",
  },
  {
    kind: "turn_boundary",
    id: "b1",
    turnId: "t1",
    status: "completed",
    durationMs: 1500,
    usage: {
      input_tokens: 12,
      output_tokens: 3,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
    },
    error: null,
    diffstat: null,
  },
];

describe("CodeTranscript", () => {
  it("renders assistant text, a tool card, a harness notice, and the turn boundary", () => {
    render(<CodeTranscript items={items} />);
    expect(screen.getByText("list the files")).toBeInTheDocument();
    expect(screen.getByText("Looking at the tree.")).toBeInTheDocument();
    expect(screen.getByRole("status", { name: "Bash succeeded" })).toBeInTheDocument();
    expect(screen.getByText("unrecognized event dropped")).toBeInTheDocument();
    expect(screen.getByText("Turn completed")).toBeInTheDocument();
    expect(screen.getByText("1.5s")).toBeInTheDocument();
    expect(screen.getByText("15 tokens")).toBeInTheDocument();
  });
});
