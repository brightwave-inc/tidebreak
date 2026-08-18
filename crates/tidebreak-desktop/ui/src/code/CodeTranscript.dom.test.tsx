// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { CodeTranscript } from "./CodeTranscript";
import type { CodeTranscriptItem } from "./CodeSessionReducer";

afterEach(() => {
  cleanup();
});

const USAGE = {
  input_tokens: 12_400,
  output_tokens: 3_100,
  cache_read_input_tokens: 900,
  cache_creation_input_tokens: 40,
};

const items: CodeTranscriptItem[] = [
  {
    kind: "user",
    id: "u1",
    turnId: "t1",
    text: "list the **files**",
    createdAt: "2026-08-15T12:00:00.000Z",
  },
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
    durationMs: 134_000,
    usage: USAGE,
    error: null,
    diffstat: { files: 2, insertions: 42, deletions: 7, truncated: false },
  },
];

describe("CodeTranscript", () => {
  it("renders assistant text, a tool card, and a harness notice", () => {
    render(<CodeTranscript items={items} />);
    expect(screen.getByText("Looking at the tree.")).toBeInTheDocument();
    expect(
      screen.getByRole("status", { name: "Bash succeeded" }),
    ).toBeInTheDocument();
    expect(screen.getByText("unrecognized event dropped")).toBeInTheDocument();
  });

  it("renders the prompt as markdown with a timestamped footer", () => {
    render(<CodeTranscript items={items} />);
    const prompt = screen.getByRole("article", { name: "You" });
    expect(within(prompt).getByText("files").tagName).toBe("STRONG");
    expect(
      screen.getByText((_, element) => element?.tagName === "TIME"),
    ).toHaveAttribute("datetime", "2026-08-15T12:00:00.000Z");
  });

  it("closes a completed turn with its duration, diffstat, and usage", () => {
    render(<CodeTranscript items={items} />);
    const seam = screen.getByRole("group", { name: "Turn finished" });
    expect(within(seam).getByText("· 2m 14s")).toBeInTheDocument();
    expect(within(seam).getByText(/2 files \+42 −7/)).toBeInTheDocument();
    expect(within(seam).getByText("12k in / 3k out")).toBeInTheDocument();
  });

  it("says why a failed turn failed", () => {
    render(
      <CodeTranscript
        items={[
          {
            kind: "turn_boundary",
            id: "b2",
            turnId: "t2",
            status: "failed",
            durationMs: 4_000,
            usage: null,
            error: "claude exited with status 1",
            diffstat: null,
          },
        ]}
      />,
    );
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Turn failed");
    expect(alert).toHaveTextContent("claude exited with status 1");
    expect(alert).toHaveTextContent("4s");
  });

  it("holds the transcript's shape until the session hydrates", () => {
    const { container, rerender } = render(
      <CodeTranscript items={[]} hydrated={false} />,
    );
    expect(container.querySelector(".animate-pulse")).not.toBeNull();
    expect(screen.queryByText("Send a message to start a turn.")).toBeNull();

    rerender(<CodeTranscript items={[]} hydrated />);
    expect(container.querySelector(".animate-pulse")).toBeNull();
    expect(
      screen.getByText("Send a message to start a turn."),
    ).toBeInTheDocument();
  });

  it("clamps a running tool's output and offers a copy control", async () => {
    const preview = Array.from(
      { length: 12 },
      (_, index) => `line ${index + 1}`,
    ).join("\n");
    render(
      <CodeTranscript
        items={[
          {
            kind: "tool",
            id: "tool-long",
            turnId: "t1",
            callId: "c1",
            name: "Bash",
            detail: { kind: "command", cmd: "seq 12", cwd: "/tmp" },
            status: "running",
            preview,
          },
        ]}
      />,
    );

    const body = screen.getByLabelText("Output");
    expect(body.textContent).toContain("line 8");
    expect(body.textContent).not.toContain("line 9");
    expect(screen.getByRole("button", { name: "Copy output" })).toBeTruthy();

    await userEvent.click(
      screen.getByRole("button", { name: "Show 4 more lines" }),
    );
    expect(screen.getByLabelText("Output").textContent).toContain("line 12");
  });

  it("shows the engine working only where nothing else says so", () => {
    const { rerender } = render(<CodeTranscript items={items} busy />);
    expect(screen.getByText("Working")).toBeInTheDocument();

    rerender(
      <CodeTranscript
        items={[
          {
            kind: "assistant",
            id: "a2",
            turnId: "t1",
            text: "Reading",
            streaming: true,
          },
        ]}
        busy
      />,
    );
    expect(screen.queryByText("Working")).toBeNull();
  });
});
