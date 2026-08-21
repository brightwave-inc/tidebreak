import { describe, expect, it, vi } from "vitest";

import {
  copyAgentRunDebug,
  fetchAgentRunProgress,
  formatAgentRunDebugReport,
} from "./AgentRunDebugReport";
import type { AgentActivityHistoryEntry, AgentRun } from "./api";

function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    id: "run-1",
    parent_id: null,
    tier: "background",
    execution_location: "container",
    code_execution_provider: "local",
    status: "failed",
    model_steps: 0,
    usage: {
      input_tokens: 0,
      output_tokens: 0,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
    },
    task: "Summarize the quarterly report",
    started_at: "2026-08-01T10:00:00Z",
    finished_at: "2026-08-01T10:04:00Z",
    last_error_code: "SANDBOX_EXIT_1",
    activity: null,
    submitted_outputs: [],
    terminal_text: null,
    created_at: "2026-08-01T09:59:00Z",
    updated_at: "2026-08-01T10:04:00Z",
    spawn_call_id: "call-7",
    ...overrides,
  };
}

describe("formatAgentRunDebugReport", () => {
  it("renders the run fields, activity timeline, and progress sections", () => {
    const activity: AgentActivityHistoryEntry[] = [
      {
        kind: "exec",
        outcome: "failed",
        at: "2026-08-01T10:01:00Z",
        detail: {
          kind: "exec",
          command: "python",
          args: ["analyze.py"],
          exit_code: 1,
          output: "Traceback: boom",
        },
      },
      {
        kind: "web_search",
        outcome: "completed",
        at: "2026-08-01T10:02:00Z",
        detail: { kind: "search", query: "quarterly revenue figures" },
      },
    ];

    const report = formatAgentRunDebugReport({
      run: run(),
      activity,
      progress: [
        {
          sequence: 1,
          text: "Reading the spreadsheet",
          at: "2026-08-01T10:00:30Z",
        },
        {
          sequence: 2,
          text: "Analysis script failed, retrying",
          at: "2026-08-01T10:01:30Z",
        },
      ],
    });

    expect(report).toContain("# Background agent run debug info");
    expect(report).toContain("- Run ID: run-1");
    expect(report).toContain("- Status: failed");
    expect(report).toContain("- Last error code: SANDBOX_EXIT_1");
    expect(report).toContain("- Task: Summarize the quarterly report");

    expect(report).toContain("## Activity");
    expect(report).toContain(
      "1. **A command failed** — failed · 2026-08-01T10:01:00Z",
    );
    expect(report).toContain("- Command: `python analyze.py`");
    expect(report).toContain("- Exit code: 1");
    expect(report).toContain("Traceback: boom");
    expect(report).toContain(
      "2. **Searched the web** — completed · 2026-08-01T10:02:00Z",
    );
    expect(report).toContain("- Query: quarterly revenue figures");

    expect(report).toContain("## Progress");
    expect(report).toContain(
      "- 2026-08-01T10:00:30Z — Reading the spreadsheet",
    );
    expect(report).toContain(
      "- 2026-08-01T10:01:30Z — Analysis script failed, retrying",
    );
  });

  it("notes a failed fetch in place of each section that could not load", () => {
    const report = formatAgentRunDebugReport({
      run: run({ status: "completed", last_error_code: null }),
      activity: null,
      progress: null,
    });

    expect(report).toContain("_Activity history could not be fetched._");
    expect(report).toContain("_Progress lines could not be fetched._");
  });

  it("includes the terminal result only when the run settled with one", () => {
    const settled = formatAgentRunDebugReport({
      run: run({
        status: "completed",
        last_error_code: null,
        terminal_text: "Done.",
      }),
      activity: [],
      progress: [],
    });
    expect(settled).toContain("## Result");
    expect(settled).toContain("Done.");

    const live = formatAgentRunDebugReport({
      run: run({ status: "running", last_error_code: null }),
      activity: [],
      progress: [],
    });
    expect(live).not.toContain("## Result");
    expect(live).toContain("_No recorded activity._");
    expect(live).toContain("_No progress published._");
  });
});

describe("fetchAgentRunProgress", () => {
  it("pages with each page's next cursor until a page arrives empty", async () => {
    const listPage = vi.fn(async (afterSequence: number) => {
      if (afterSequence === 0) {
        return {
          entries: [
            { sequence: 1, text: "one", at: "t1" },
            { sequence: 2, text: "two", at: "t2" },
          ],
          nextSequence: 2,
        };
      }
      if (afterSequence === 2) {
        return {
          entries: [{ sequence: 3, text: "three", at: "t3" }],
          nextSequence: 3,
        };
      }
      return { entries: [], nextSequence: 3 };
    });

    const entries = await fetchAgentRunProgress(listPage);

    expect(entries.map((entry) => entry.text)).toEqual(["one", "two", "three"]);
    expect(listPage.mock.calls.map(([after]) => after)).toEqual([0, 2, 3]);
  });
});

describe("copyAgentRunDebug", () => {
  it("copies the report and degrades a failed section fetch instead of failing", async () => {
    const written: string[] = [];
    const notices: { message: string; description?: string }[] = [];
    await copyAgentRunDebug(run(), {
      fetchActivity: vi.fn(async () => {
        throw new Error("boom");
      }),
      fetchProgress: vi.fn(async () => [
        { sequence: 1, text: "working", at: "t1" },
      ]),
      writeClipboard: async (text) => {
        written.push(text);
      },
      notify: (notice) => notices.push(notice),
    });

    expect(written).toHaveLength(1);
    expect(written[0]).toContain("_Activity history could not be fetched._");
    expect(written[0]).toContain("working");
    expect(notices[0]?.message).toBe("Debug info copied");
  });

  it("surfaces a clipboard failure as one error notice", async () => {
    const notices: { message: string; description?: string }[] = [];
    await copyAgentRunDebug(run(), {
      fetchActivity: vi.fn(async () => []),
      fetchProgress: vi.fn(async () => []),
      writeClipboard: vi.fn(async () => {
        throw new Error("clipboard unavailable");
      }),
      notify: (notice) => notices.push(notice),
    });

    expect(notices).toEqual([{ message: "clipboard unavailable" }]);
  });
});
