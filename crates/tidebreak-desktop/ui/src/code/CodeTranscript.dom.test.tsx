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
    startedAt: "2026-08-15T12:00:00.000Z",
    durationMs: 1_200,
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
      screen.getByRole("status", { name: "Command run succeeded" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Command run")).toBeInTheDocument();
    expect(screen.getByText("ls")).toBeInTheDocument();
    expect(screen.getByText("1.2s")).toBeInTheDocument();
    expect(screen.queryByLabelText("Output")).toBeNull();
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
    expect(alert).toHaveTextContent("4.0s");
  });

  it("falls back to the tool name when the engine sends no target", () => {
    render(
      <CodeTranscript
        items={[
          {
            kind: "tool",
            id: "tool-bare",
            turnId: "t1",
            callId: "c4",
            name: "Read",
            // Harnesses open a call before its arguments finish streaming, so
            // the path can arrive empty and never be restated.
            detail: { kind: "file_read", path: "" },
            status: "running",
            preview: "",
            startedAt: null,
            durationMs: null,
          },
        ]}
      />,
    );
    const line = screen.getByRole("button", { name: /File read/ });
    expect(line).toHaveTextContent("File read");
    expect(line).toHaveTextContent("Read");
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

  it("expands a failed tool, clamps long output, and stops the transcript following", async () => {
    const preview = Array.from(
      { length: 13 },
      (_, index) => `line ${index + 1}`,
    ).join("\n");
    const reveals: number[] = [];
    render(
      <CodeTranscript
        onReveal={() => reveals.push(1)}
        items={[
          {
            kind: "tool",
            id: "tool-long",
            turnId: "t1",
            callId: "c1",
            name: "Bash",
            detail: { kind: "command", cmd: "seq 13", cwd: "/tmp" },
            status: "failed",
            preview,
            startedAt: null,
            durationMs: 800,
          },
        ]}
      />,
    );

    const body = screen.getByLabelText("Output");
    expect(body.textContent).toContain("line 12");
    expect(body.textContent).not.toContain("line 13");
    expect(screen.getByRole("button", { name: "Copy output" })).toBeTruthy();

    await userEvent.click(
      screen.getByRole("button", { name: "· · · 1 more line" }),
    );
    expect(screen.getByLabelText("Output").textContent).toContain("line 13");
    // Growing the column under the reader's cursor must not drag them to the
    // tail, so every reveal reaches the host that owns the scroll.
    expect(reveals).toHaveLength(1);
  });

  it("keeps a successful call closed and names a denial with the constant verb", () => {
    render(
      <CodeTranscript
        items={[
          {
            kind: "tool",
            id: "tool-denied",
            turnId: "t1",
            callId: "c2",
            name: "Bash",
            detail: { kind: "command", cmd: "rm -rf /", cwd: "/tmp" },
            status: "denied",
            preview: "denied by policy",
            startedAt: null,
            durationMs: null,
          },
        ]}
      />,
    );
    expect(screen.getByText("Command denied")).toBeInTheDocument();
    expect(screen.getByLabelText("Output")).toHaveTextContent("denied by policy");
  });

  it("streams the tail of a running command without showing the head", () => {
    const preview = Array.from(
      { length: 16 },
      (_, index) => `line ${index + 1}`,
    ).join("\n");
    render(
      <CodeTranscript
        items={[
          {
            kind: "tool",
            id: "tool-run",
            turnId: "t1",
            callId: "c3",
            name: "Bash",
            detail: { kind: "command", cmd: "seq 16", cwd: "/tmp" },
            status: "running",
            preview,
            startedAt: "2026-08-15T12:00:00.000Z",
            durationMs: null,
          },
        ]}
      />,
    );
    expect(screen.getByText("Command run")).toBeInTheDocument();
    const body = screen.getByLabelText("Output");
    expect(body.textContent?.split("\n")[0]).toBe("line 5");
    expect(body.textContent).toContain("line 16");
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
