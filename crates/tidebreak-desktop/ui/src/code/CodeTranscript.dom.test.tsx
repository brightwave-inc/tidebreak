// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeTranscript } from "./CodeTranscript";
import type { CodeApprovalSnapshot } from "../api/types";
import type { CodeTranscriptItem } from "./CodeSessionReducer";

afterEach(() => {
  cleanup();
});

const USAGE = {
  input_tokens: 12_400,
  output_tokens: 3_100,
  cache_read_input_tokens: 900,
  cache_creation_input_tokens: 40,
  context_tokens: 13_340,
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
    parentCallId: null,
    text: "Looking at the tree.",
    streaming: false,
  },
  {
    kind: "tool",
    id: "tool1",
    turnId: "t1",
    callId: "c1",
    parentCallId: null,
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

type CodeToolItem = Extract<CodeTranscriptItem, { kind: "tool" }>;

/**
 * A burst of three calls, the way an engine actually works: search, read,
 * read. The last one carries the run's status.
 */
function run(last: "running" | "succeeded"): CodeToolItem[] {
  return [
    {
      kind: "tool",
      id: "run-1",
      turnId: "t1",
      callId: "r1",
      parentCallId: null,
      name: "Grep",
      detail: { kind: "search", query: "Fenced|reap" },
      status: "succeeded",
      preview: "3 matches",
      startedAt: null,
      durationMs: 400,
    },
    {
      kind: "tool",
      id: "run-2",
      turnId: "t1",
      callId: "r2",
      parentCallId: null,
      name: "Read",
      detail: { kind: "file_read", path: "docs/code-mode.md" },
      status: "succeeded",
      preview: "",
      startedAt: null,
      durationMs: 600,
    },
    {
      kind: "tool",
      id: "run-3",
      turnId: "t1",
      callId: "r3",
      parentCallId: null,
      name: "Read",
      detail: { kind: "file_read", path: "src/code/recovery.rs" },
      status: last,
      preview: "",
      // Null so a running row draws no elapsed label: the transcript reads
      // the real clock, and a fixed fixture timestamp would age into nonsense.
      startedAt: null,
      durationMs: last === "running" ? null : 1_200,
    },
  ];
}

describe("CodeTranscript", () => {
  it("renders assistant text, a tool card, and a harness notice", () => {
    render(<CodeTranscript items={items} />);
    expect(screen.getByText("Looking at the tree.")).toBeInTheDocument();
    // The line's outcome belongs to its own name. It is deliberately not a
    // live region: see the streaming test below.
    expect(
      screen.getByRole("button", { name: /Command run.*succeeded/ }),
    ).toBeInTheDocument();
    expect(screen.getByText("Command run")).toBeInTheDocument();
    expect(screen.getByText("ls")).toBeInTheDocument();
    expect(screen.getByText("1.2s")).toBeInTheDocument();
    expect(screen.queryByLabelText("Output")).toBeNull();
    expect(screen.getByText("unrecognized event dropped")).toBeInTheDocument();
  });

  it("shows the command beside the verb, without its worktree cd prefix", () => {
    const item: CodeTranscriptItem = {
      kind: "tool",
      id: "tool-cd",
      turnId: "t1",
      callId: "c2",
      parentCallId: null,
      name: "Bash",
      detail: {
        kind: "command",
        cmd: 'cd "/Users/me/Library/Application Support/dev/worktrees/abc" && cargo test -p core',
        cwd: "/tmp",
      },
      status: "succeeded",
      preview: "",
      startedAt: "2026-08-15T12:00:00.000Z",
      durationMs: 900,
    };
    render(<CodeTranscript items={[item]} />);
    // The leading `cd <worktree> &&` repeats the session's own directory on
    // every row and pushes the real command into the truncated middle.
    const subject = screen.getByText("cargo test -p core");
    expect(screen.queryByText(/Application Support/)).toBeNull();
    // The verb and the command are direct children of the flex title row.
    // An inline wrapper span here once pushed the command onto its own line
    // and broke middle truncation (#2282 regression).
    const verb = screen.getByText("Command run");
    expect(subject.parentElement).toBe(verb.parentElement);
    expect(verb.parentElement?.className).toContain("flex");
  });

  it("announces the turn's lifecycle and nothing that streams", async () => {
    // A tool line changes on every streamed byte. Wrapping it in a live region
    // reads the whole session out, over and over, and buries the one thing a
    // supervisor has to hear. The turn's start and end go through one region;
    // the output goes through none.
    const running: CodeTranscriptItem[] = [
      items[0],
      {
        kind: "tool",
        id: "tool-live",
        turnId: "t1",
        callId: "c9",
        parentCallId: null,
        name: "Bash",
        detail: { kind: "command", cmd: "seq 3", cwd: "/tmp" },
        status: "running",
        preview: "line 1\nline 2",
        startedAt: "2026-08-15T12:00:00.000Z",
        durationMs: null,
      },
    ];
    // The region is mounted empty, so reopening a finished session does not
    // read its last outcome at the reader before they ask for anything.
    const { rerender } = render(<CodeTranscript items={[items[0]]} />);
    expect(screen.getByTestId("code-turn-announcer")).toHaveTextContent("");

    rerender(<CodeTranscript items={running} busy />);
    const tool = screen.getByRole("button", { name: /Command run.*running/ });
    expect(screen.queryByLabelText("Output")).toBeNull();
    await userEvent.click(tool);
    expect(
      screen
        .getByLabelText("Output")
        .closest('[aria-live], [role="status"], [role="alert"]'),
    ).toBeNull();
    expect(screen.getByTestId("code-turn-announcer")).toHaveTextContent(
      "Turn running",
    );

    rerender(<CodeTranscript items={items} />);
    expect(screen.getByTestId("code-turn-announcer")).toHaveTextContent(
      "Turn finished · 2m 14s",
    );
  });

  it("leaves a failed turn to its alert rather than saying it twice", () => {
    const failed: CodeTranscriptItem[] = [
      items[0],
      {
        kind: "turn_boundary",
        id: "b3",
        turnId: "t1",
        status: "failed",
        durationMs: 4_000,
        usage: null,
        error: "claude exited with status 1",
        diffstat: null,
      },
    ];
    const { rerender } = render(<CodeTranscript items={[items[0]]} busy />);
    rerender(<CodeTranscript items={failed} />);
    expect(screen.getByRole("alert")).toHaveTextContent("Turn failed");
    expect(screen.getByTestId("code-turn-announcer")).toHaveTextContent("");
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
    expect(within(seam).queryByText(/in \//)).not.toBeInTheDocument();
  });

  /**
   * The seam is the fork point: "Fork from here" must hand the host this
   * boundary's turn id, and the affordance must not render at all for a
   * host that offers no fork — a menu with nothing in it is worse than none.
   */
  it("forks from a turn's seam menu", async () => {
    const user = userEvent.setup();
    const onForkFromTurn = vi.fn();
    render(<CodeTranscript items={items} onForkFromTurn={onForkFromTurn} />);

    await user.click(screen.getByRole("button", { name: "Turn actions" }));
    await user.click(screen.getByRole("menuitem", { name: /Fork from here/ }));

    expect(onForkFromTurn).toHaveBeenCalledWith("t1");
  });

  it("offers no turn actions without a fork handler", () => {
    render(<CodeTranscript items={items} />);
    expect(
      screen.queryByRole("button", { name: "Turn actions" }),
    ).not.toBeInTheDocument();
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
            parentCallId: null,
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

  it("shows Codex shell commands without launcher and quote-escape noise", () => {
    render(
      <CodeTranscript
        items={[
          {
            kind: "tool",
            id: "tool-shell",
            turnId: "t1",
            callId: "c5",
            parentCallId: null,
            name: "commandExecution",
            detail: {
              kind: "command",
              cmd: String.raw`/bin/zsh -lc "rg --files -g '"'!node_modules'"' && printf '\\nDone\\n'"`,
              cwd: "/tmp",
            },
            status: "succeeded",
            preview: "",
            startedAt: null,
            durationMs: null,
          },
        ]}
      />,
    );

    const line = screen.getByRole("button", { name: /Command run.*succeeded/ });
    expect(line).toHaveTextContent(
      String.raw`rg --files -g '!node_modules' && printf '\nDone\n'`,
    );
    expect(line).not.toHaveTextContent("/bin/zsh -lc");
    expect(line).not.toHaveTextContent(`'"'`);
  });

  it("keeps a folded multi-line command on one visible line", () => {
    const command = "python3 - <<'PY'\nprint('ok')\nPY";
    render(
      <CodeTranscript
        items={[
          {
            kind: "tool",
            id: "tool-heredoc",
            turnId: "t1",
            callId: "c6",
            parentCallId: null,
            name: "commandExecution",
            detail: { kind: "command", cmd: command, cwd: "/tmp" },
            status: "succeeded",
            preview: "ok",
            startedAt: null,
            durationMs: null,
          },
        ]}
      />,
    );

    const line = screen.getByRole("button", {
      name: /Command run.*succeeded/,
    });
    const subject = line.querySelector("[title]");
    expect(subject).toHaveAttribute("title", command);
    expect(subject).toHaveTextContent("python3 - <<'PY' print('ok') PY");
    expect(subject?.textContent).not.toMatch(/[\r\n]/);
    expect(line).toHaveAttribute("aria-expanded", "false");
  });

  it("holds the transcript's shape until the session hydrates", () => {
    const { container, rerender } = render(
      <CodeTranscript items={items} hydrated={false} />,
    );
    expect(container.querySelector(".animate-pulse")).not.toBeNull();
    expect(screen.queryByText("Send a message to start a turn.")).toBeNull();
    expect(screen.queryByText("Looking at the tree.")).toBeNull();

    rerender(<CodeTranscript items={items} hydrated />);
    expect(container.querySelector(".animate-pulse")).toBeNull();
    expect(screen.getByText("Looking at the tree.")).toBeInTheDocument();
  });

  it("keeps a failed tool folded until the reader opens it", async () => {
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
            parentCallId: null,
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

    const line = screen.getByRole("button", { name: /Command run.*failed/ });
    expect(line).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByLabelText("Output")).toBeNull();

    await userEvent.click(line);
    const body = screen.getByLabelText("Output");
    expect(body.textContent).toContain("line 12");
    expect(body.textContent).not.toContain("line 13");
    expect(screen.getByRole("button", { name: "Copy output" })).toBeTruthy();
    expect(reveals).toHaveLength(1);

    await userEvent.click(
      screen.getByRole("button", { name: "· · · 1 more line" }),
    );
    expect(screen.getByLabelText("Output").textContent).toContain("line 13");
    // Growing the column under the reader's cursor must not drag them to the
    // tail, so every reveal reaches the host that owns the scroll.
    expect(reveals).toHaveLength(2);

    // Collapsing the line leaves the reader on the line they collapsed: the
    // toggle is the row itself, so there is nothing for focus to fall out of.
    expect(line).toHaveAttribute("aria-expanded", "true");
    await userEvent.click(line);
    expect(line).toHaveAttribute("aria-expanded", "false");
    expect(line).toHaveFocus();
  });

  it("keeps a denial closed and names it with the constant verb", async () => {
    render(
      <CodeTranscript
        items={[
          {
            kind: "tool",
            id: "tool-denied",
            turnId: "t1",
            callId: "c2",
            parentCallId: null,
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
    expect(screen.queryByLabelText("Output")).toBeNull();
    await userEvent.click(
      screen.getByRole("button", { name: /Command denied.*denied/ }),
    );
    expect(screen.getByLabelText("Output")).toHaveTextContent(
      "denied by policy",
    );
  });

  it("keeps a running command folded until the reader opens its tail", async () => {
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
            parentCallId: null,
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
    const line = screen.getByRole("button", { name: /Command run.*running/ });
    expect(line).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByLabelText("Output")).toBeNull();

    await userEvent.click(line);
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
            parentCallId: null,
            text: "Reading",
            streaming: true,
          },
        ]}
        busy
      />,
    );
    expect(screen.queryByText("Working")).toBeNull();
  });

  it("hangs a parked approval off the tool line it is waiting on", () => {
    // Nothing on the wire links the two: the card and the still-running line
    // above it show the same command, and only their adjacency says so. A
    // decided approval, or one that follows anything else, stands alone.
    const parked: CodeTranscriptItem[] = [
      items[0],
      {
        kind: "tool",
        id: "tool-parked",
        turnId: "t1",
        callId: "c7",
        parentCallId: null,
        name: "Bash",
        detail: { kind: "command", cmd: "rm -rf /tmp/scratch", cwd: "/tmp" },
        status: "running",
        preview: "",
        startedAt: "2026-08-15T12:00:00.000Z",
        durationMs: null,
      },
      {
        kind: "approval",
        id: "approval:a7",
        approvalId: "a7",
        state: "pending",
      },
    ];
    const approval: CodeApprovalSnapshot = {
      id: "a7",
      session_id: "s1",
      turn_id: "t1",
      kind: { type: "command", cmd: "rm -rf /tmp/scratch", cwd: "/tmp" },
      harness_raw_json: "{}",
      state: "pending",
      requested_at: "2026-08-15T12:00:00.000Z",
    };

    const { rerender } = render(
      <CodeTranscript items={parked} approvals={{ a7: approval }} />,
    );
    expect(
      document.querySelector('[data-code-approval-attached="true"]'),
    ).not.toBeNull();

    rerender(
      <CodeTranscript
        items={[items[0], parked[2]!]}
        approvals={{ a7: approval }}
      />,
    );
    expect(
      document.querySelector('[data-code-approval-attached="true"]'),
    ).toBeNull();
  });

  it("folds a run of calls behind its newest line and opens to the rows", async () => {
    // A burst of calls is not what the reader came for, and one row per call
    // is the whole viewport. The line names the call still running, because
    // that is the one worth watching.
    render(<CodeTranscript items={[...run("running")]} />);

    expect(screen.getByText("+2 more")).toBeInTheDocument();
    const line = screen.getByRole("button", {
      name: /File read.*recovery\.rs.*and 2 more.*running/,
    });
    expect(line).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("Fenced|reap")).toBeNull();

    await userEvent.click(line);
    expect(line).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("Fenced|reap")).toBeInTheDocument();
    expect(screen.getByText("docs/code-mode.md")).toBeInTheDocument();
  });

  it("names the last call and totals the run once it settles", () => {
    render(<CodeTranscript items={[...run("succeeded")]} />);

    expect(
      screen.getByRole("button", {
        name: /File read.*recovery\.rs.*and 2 more.*succeeded/,
      }),
    ).toBeInTheDocument();
    // 400ms + 600ms + 1200ms of sequential work, said once.
    expect(screen.getByText("2.2s")).toBeInTheDocument();
  });

  it("keeps a run with a failure folded until the reader opens it", async () => {
    const middle = run("succeeded")[1]!;
    const items: CodeToolItem[] = [
      run("succeeded")[0]!,
      { ...middle, status: "failed", preview: "no such file" },
      run("succeeded")[2]!,
    ];
    render(<CodeTranscript items={items} />);

    const group = screen.getByRole("button", { name: /and 2 more.*failed/ });
    expect(group).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("no such file")).toBeNull();

    await userEvent.click(group);
    const failed = screen.getByRole("button", {
      name: /File read.*docs\/code-mode\.md.*failed/,
    });
    expect(failed).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(failed);
    expect(screen.getByText("no such file")).toBeInTheDocument();
  });

  it("holds a parked call out of the group it would end", () => {
    const parked: CodeTranscriptItem[] = [
      ...run("succeeded").slice(0, 2),
      {
        kind: "tool",
        id: "tool-parked",
        turnId: "t1",
        callId: "c9",
        parentCallId: null,
        name: "Bash",
        detail: { kind: "command", cmd: "rm -rf /tmp/scratch", cwd: "/tmp" },
        status: "running",
        preview: "",
        startedAt: "2026-08-15T12:00:00.000Z",
        durationMs: null,
      },
      {
        kind: "approval",
        id: "approval:a9",
        approvalId: "a9",
        state: "pending",
      },
    ];
    render(
      <CodeTranscript
        items={parked}
        approvals={{
          a9: {
            id: "a9",
            session_id: "s1",
            turn_id: "t1",
            kind: { type: "command", cmd: "rm -rf /tmp/scratch", cwd: "/tmp" },
            harness_raw_json: "{}",
            state: "pending",
            requested_at: "2026-08-15T12:00:00.000Z",
          },
        }}
      />,
    );

    // The two settled calls still group; the parked one is held out of it,
    // because the card below repeats the command that row shows.
    expect(screen.getByText("+1 more")).toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: /Command run.*rm -rf \/tmp\/scratch.*running/,
      }),
    ).toBeInTheDocument();
    expect(
      document.querySelector('[data-code-approval-attached="true"]'),
    ).not.toBeNull();
  });

  it("keeps a folded thought line between the groups it separates", async () => {
    // Where the engine paused is part of the story, so reasoning breaks a run
    // rather than disappearing into one. It arrives folded: this surface
    // thinks between every pair of calls, and blocks that open themselves
    // stack until they are the whole turn.
    render(
      <CodeTranscript
        items={[
          ...run("succeeded").slice(0, 2),
          {
            kind: "reasoning",
            id: "think-1",
            turnId: "t1",
            text: "Now check what identity we store for the child.",
            streaming: true,
          },
          ...run("succeeded")
            .slice(0, 2)
            .map((tool, index) => ({ ...tool, id: `late-${index}` })),
        ]}
      />,
    );

    expect(screen.getAllByText("+1 more")).toHaveLength(2);
    const thought = screen.getByRole("button", { name: /Thinking/i });
    expect(thought).toHaveAttribute("data-state", "closed");
    expect(
      screen.queryByText("Now check what identity we store for the child."),
    ).not.toBeInTheDocument();

    await userEvent.click(thought);
    expect(
      screen.getByText("Now check what identity we store for the child."),
    ).toBeInTheDocument();
  });
  it("shows the session recap on the newest turn boundary only", () => {
    const boundary = (id: string, turnId: string): CodeTranscriptItem => ({
      kind: "turn_boundary",
      id,
      turnId,
      status: "completed",
      durationMs: 4_000,
      usage: USAGE,
      error: null,
      diffstat: null,
    });
    const recap = "Retry test passes. Next: fold the backoff into refresh.";
    render(
      <CodeTranscript
        items={[boundary("b1", "t1"), boundary("b2", "t2")]}
        recap={recap}
      />,
    );

    // One copy, not one per turn: the line describes where the session stands,
    // so an older boundary carrying it would be stale the moment it rendered.
    expect(screen.getAllByText(recap)).toHaveLength(1);
  });
});
