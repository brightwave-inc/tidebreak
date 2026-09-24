// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeReviewSnapshot, HarnessDoctorEntry } from "../../api/types";
import { harnessDoctor } from "../../stories/fixtures";
import { CommentCard } from "../diff/DiffComments";
import type { ReviewComment } from "../diff/reviewComments";
import { reviewRowLabel } from "../TurnReviewCard";
import { ReviewChangesForm, ReviewStatus, WORKING_TREE } from "./ReviewChanges";
import { reviewEngineChoices } from "./reviewEngines";
import { LOST_REVIEW_MESSAGE, lostReview } from "./reviewStore";

afterEach(cleanup);

function entry(
  overrides: Partial<HarnessDoctorEntry> & Pick<HarnessDoctorEntry, "kind">,
): HarnessDoctorEntry {
  const base = harnessDoctor.harnesses[0]!;
  return { ...base, found: true, authenticated: true, ...overrides };
}

function running(
  overrides: Partial<CodeReviewSnapshot> = {},
): CodeReviewSnapshot {
  return {
    id: "rev-1",
    workspace_id: "ws-1",
    session_id: "sess-1",
    harness: "codex",
    model: "gpt-5.5",
    permission_mode: "plan",
    status: "running",
    progress: {
      tool_calls: 4,
      files_read: 3,
      refused: 1,
      activity: "Reading src/queue.ts",
    },
    started_at: "2026-09-24T10:00:00.000Z",
    ...overrides,
  };
}

const NOW = Date.parse("2026-09-24T10:01:05.000Z");

describe("the Review changes form", () => {
  it("offers the engines that can review, says why the others cannot, and starts on request", async () => {
    const onStart = vi.fn();
    render(
      <ReviewChangesForm
        choices={reviewEngineChoices([
          entry({ kind: "claude_code" }),
          entry({ kind: "codex" }),
        ])}
        author="claude_code"
        engine="codex"
        onEngineChange={() => {}}
        modelOptions={[
          {
            id: "gpt-5.5",
            label: "GPT-5.5",
            source: "Codex CLI",
            default: true,
          },
        ]}
        model="gpt-5.5"
        scopes={[
          { value: "turn-3", label: "Turn 3" },
          { value: WORKING_TREE, label: "Working tree" },
        ]}
        scope="turn-3"
        onScopeChange={() => {}}
        onStart={onStart}
        onCancel={() => {}}
      />,
    );
    expect(screen.getByRole("combobox")).toHaveTextContent("Codex CLI");
    expect(screen.getByRole("radio", { name: "Turn 3" })).toBeChecked();
    expect(screen.getByText("GPT-5.5")).toBeInTheDocument();
    // It says what it reviews, and that later edits are not part of it.
    expect(
      screen.getByText(
        /It can\s+read what your account can read, as coding engines can/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        /Edits you make after you start aren't part of the review/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        /can't change your files or reach the network beyond its own\s+model/,
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/cannot edit your files/)).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Start review" }));
    expect(onStart).toHaveBeenCalledTimes(1);
  });

  it("says when the author's engine reviews its own changes because none other is ready", () => {
    render(
      <ReviewChangesForm
        choices={reviewEngineChoices([
          entry({ kind: "claude_code" }),
          entry({ kind: "codex", authenticated: false }),
        ])}
        author="claude_code"
        engine="claude_code"
        onEngineChange={() => {}}
        modelOptions={[]}
        scopes={[{ value: WORKING_TREE, label: "Working tree" }]}
        scope={WORKING_TREE}
        onScopeChange={() => {}}
        onStart={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(
      screen.getByText(
        "No other engine is ready, so Claude Code reviews its own changes.",
      ),
    ).toBeInTheDocument();
  });

  it("cannot start without an engine that can review", () => {
    render(
      <ReviewChangesForm
        choices={reviewEngineChoices([entry({ kind: "codex", found: false })])}
        author="claude_code"
        engine={null}
        onEngineChange={() => {}}
        modelOptions={[]}
        scopes={[{ value: WORKING_TREE, label: "Working tree" }]}
        scope={WORKING_TREE}
        onScopeChange={() => {}}
        onStart={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(screen.getByRole("button", { name: "Start review" })).toBeDisabled();
    expect(screen.getByText(/No engine can review right now/)).toBeVisible();
  });
});

describe("the review's status", () => {
  it("shows what a running reviewer is doing, for how long, and stops it", async () => {
    const onStop = vi.fn();
    render(<ReviewStatus review={running()} now={NOW} onStop={onStop} />);
    expect(screen.getByRole("status")).toHaveTextContent(
      "Codex CLI is reviewing the working tree",
    );
    expect(screen.getByText("gpt-5.5 · 1m 5s")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Reading src/queue.ts · 3 files read · 1 change refused",
      ),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Stop review" }));
    expect(onStop).toHaveBeenCalledTimes(1);
    expect(
      screen.queryByRole("button", { name: "Close the review status" }),
    ).toBeNull();
  });

  it("reads as stopping, and takes no second stop, until the review ends", () => {
    const onStop = vi.fn();
    render(
      <ReviewStatus review={running()} now={NOW} stopping onStop={onStop} />,
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Stopping Codex CLI's review",
    );
    const stop = screen.getByRole("button", { name: "Stopping…" });
    expect(stop).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Stop review" })).toBeNull();
  });

  it("says a review stopped when Tidebreak restarted, and offers to run it again", async () => {
    const onRetry = vi.fn();
    render(
      <ReviewStatus
        review={lostReview(running(), () => "2026-09-24T10:02:00.000Z")}
        now={NOW}
        onRetry={onRetry}
        onClose={() => {}}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Codex CLI couldn't finish the review",
    );
    expect(screen.getByText(LOST_REVIEW_MESSAGE)).toBeInTheDocument();
    expect(
      screen.queryByText("Reading src/queue.ts", { exact: false }),
    ).toBeNull();
    expect(screen.queryByRole("button", { name: "Stop review" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("says why a review failed, and offers the way back", async () => {
    const onRetry = vi.fn();
    const onOpenEngines = vi.fn();
    render(
      <ReviewStatus
        review={running({
          status: "failed",
          finished_at: "2026-09-24T10:00:30.000Z",
          failure: {
            kind: "signed_out",
            message:
              "Codex CLI is not signed in, or its sign-in was refused. Sign in from Settings > Coding engines.",
          },
        })}
        now={NOW}
        onRetry={onRetry}
        onOpenEngines={onOpenEngines}
        onClose={() => {}}
      />,
    );
    // The headline says what came of it; the detail alone says why, so the
    // two never repeat each other.
    expect(screen.getByRole("status")).toHaveTextContent(
      "Codex CLI couldn't finish the review",
    );
    expect(
      screen.getByText(
        "Codex CLI is not signed in, or its sign-in was refused. Sign in from Settings > Coding engines.",
      ),
    ).toBeInTheDocument();
    expect(screen.getAllByText(/is not signed in/)).toHaveLength(1);
    await userEvent.click(
      screen.getByRole("button", { name: "Coding engines" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(onOpenEngines).toHaveBeenCalledTimes(1);
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("reads a stopped review and one that ran out of time plainly", () => {
    const { rerender } = render(
      <ReviewStatus review={running({ status: "cancelled" })} now={NOW} />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Review stopped");
    rerender(
      <ReviewStatus
        review={running({
          status: "timed_out",
          failure: {
            kind: "timed_out",
            message: "Codex CLI ran past 20 minutes and was stopped",
          },
        })}
        now={NOW}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Codex CLI couldn't finish the review",
    );
    expect(
      screen.getByText("Codex CLI ran past 20 minutes and was stopped"),
    ).toBeInTheDocument();
  });

  it("counts findings, and offers to keep or dismiss the ones waiting", async () => {
    const onKeepAll = vi.fn();
    render(
      <ReviewStatus
        review={running({
          status: "completed",
          result: {
            summary: "One real bug.",
            findings: [
              {
                path: "a.ts",
                start_line: 1,
                end_line: 1,
                severity: "high",
                title: "Bug",
                explanation: "Fix it.",
              },
            ],
            unplaced: [],
            rejected: 2,
            diff: "",
          },
        })}
        now={NOW}
        proposed={1}
        onKeepAll={onKeepAll}
        onDismissAll={() => {}}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Codex CLI reported 1 finding",
    );
    expect(screen.getByText("One real bug.")).toBeInTheDocument();
    expect(
      screen.getByText(/2 entries in the answer could not be read/),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Keep all" }));
    expect(onKeepAll).toHaveBeenCalledTimes(1);
  });
});

describe("a reviewer's finding in the diff", () => {
  const finding: ReviewComment = {
    id: "f1",
    author: { kind: "reviewer", engine: "codex", reviewId: "rev-1" },
    path: "src/queue.ts",
    lines: [
      { kind: "add", oldNo: null, newNo: 5, text: "while (true) send();" },
    ],
    body: "It never stops.",
    title: "The flush loop never stops",
    severity: "high",
    proposed: true,
    createdAt: "2026-09-24T10:00:00.000Z",
  };

  it("names its reviewer and severity, and waits to be kept or dismissed", async () => {
    const onKeep = vi.fn();
    const onDelete = vi.fn();
    render(
      <CommentCard
        comment={finding}
        label="Line 5"
        sending={false}
        onKeep={onKeep}
        onEdit={() => {}}
        onDelete={onDelete}
      />,
    );
    expect(
      screen.getByRole("article", { name: "Codex CLI Line 5" }),
    ).toBeInTheDocument();
    expect(screen.getByText("High")).toBeInTheDocument();
    expect(screen.getByText("The flush loop never stops")).toBeInTheDocument();
    expect(
      screen.getByText("Goes with your next message once kept"),
    ).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: "Keep the finding on line 5" }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Dismiss the finding on line 5" }),
    );
    expect(onKeep).toHaveBeenCalledTimes(1);
    expect(onDelete).toHaveBeenCalledTimes(1);
  });

  it("goes with the next message once kept", () => {
    const { proposed: _proposed, ...kept } = finding;
    render(
      <CommentCard
        comment={kept}
        label="Line 5"
        sending={false}
        onKeep={() => {}}
        onEdit={() => {}}
        onDelete={() => {}}
      />,
    );
    expect(screen.getByText("Goes with your next message")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Keep the finding on line 5" }),
    ).toBeNull();
  });
});

describe("the transcript's record of a review", () => {
  it("says who reviewed what, and how many findings", () => {
    expect(
      reviewRowLabel({
        harness: "codex",
        turnId: null,
        turnOrdinal: null,
        outcome: "completed",
        findings: 3,
      }),
    ).toBe("Codex CLI reviewed the working tree: 3 findings");
    expect(
      reviewRowLabel({
        harness: "grok",
        turnId: "t-2",
        turnOrdinal: 2,
        outcome: "completed",
        findings: 0,
      }),
    ).toBe("Grok CLI reviewed turn 2 and found nothing to change");
    expect(
      reviewRowLabel({
        harness: "claude_code",
        turnId: null,
        turnOrdinal: null,
        outcome: "cancelled",
        findings: 0,
      }),
    ).toBe("Stopped Claude Code's review of the working tree");
  });
});
