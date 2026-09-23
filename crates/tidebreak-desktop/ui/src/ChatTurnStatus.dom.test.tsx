// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ToolActionPreview } from "./api";
import { ChatPromptAnnouncer } from "./ChatPromptAnnouncer";
import {
  approvalAnnouncement,
  composerTurnStatus,
  promptQuestion,
} from "./chatTurnStatus";
import type { ChatTurnEnding } from "./ChatSessionReducer";
import { Composer } from "./Composer";
import type { ChatMessage } from "./MessageList";

afterEach(cleanup);

const runTests: ToolActionPreview = {
  tool: "exec",
  command: "npm",
  args: ["test"],
  cwd: ".",
  files: [],
};

const parkedOnCommand: ChatMessage[] = [
  { id: "u1", role: "user", text: "Run the tests" },
  {
    id: "tool-call-1",
    role: "tool",
    callId: "call-1",
    name: "exec",
    status: "waiting_approval",
    preview: runTests,
  },
  {
    id: "approval-call-1",
    role: "approval",
    callId: "call-1",
    summary:
      "Allow Tidebreak to run a command that leaves this work's workspace and may reach the network?",
    preview: runTests,
    canApprove: true,
    canRemember: true,
  },
];

function WorkComposer({
  messages,
  busy,
  ending = null,
}: {
  messages: ChatMessage[];
  busy: boolean;
  ending?: ChatTurnEnding | null;
}) {
  return (
    <Composer
      activeTurnId={busy ? "turn-1" : null}
      busy={busy}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft=""
      onDraftChange={vi.fn()}
      onSend={vi.fn(async () => {})}
      onSteer={vi.fn(async () => {})}
      onStop={vi.fn(async () => {})}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
      turnStatus={composerTurnStatus({
        waiting: approvalAnnouncement(messages),
        busy,
        ending,
      })}
    />
  );
}

function composerStatus() {
  const region = screen
    .getAllByRole("status")
    .find((element) => element.classList.contains("sr-only"));
  if (!region) throw new Error("the composer has no status region");
  return region;
}

describe("the Work composer's status region", () => {
  it("says what the turn waits on instead of that the agent is responding", () => {
    const { rerender } = render(
      <WorkComposer messages={parkedOnCommand.slice(0, 1)} busy />,
    );
    expect(composerStatus()).toHaveTextContent("Agent is responding");

    rerender(<WorkComposer messages={parkedOnCommand} busy />);
    expect(composerStatus()).toHaveTextContent(
      "Waiting for your approval: Run this command? npm test",
    );

    // Deciding resolves the card, and the region goes back to the turn.
    const decided = parkedOnCommand.map((message) =>
      message.role === "approval" ? { ...message, resolved: true } : message,
    );
    rerender(<WorkComposer messages={decided} busy />);
    expect(composerStatus()).toHaveTextContent("Agent is responding");
  });

  it("says how the reply ended", () => {
    const { rerender } = render(
      <WorkComposer messages={[]} busy={false} ending="finished" />,
    );
    expect(composerStatus()).toHaveTextContent("Response finished");

    rerender(<WorkComposer messages={[]} busy={false} ending="stopped" />);
    expect(composerStatus()).toHaveTextContent("Stopped");

    // A failure's notice in the transcript is already an alert.
    rerender(<WorkComposer messages={[]} busy={false} ending="failed" />);
    expect(composerStatus()).toHaveTextContent("Ready to send");

    // The next turn's start clears the ending.
    rerender(<WorkComposer messages={[]} busy ending={null} />);
    expect(composerStatus()).toHaveTextContent("Agent is responding");
  });
});

describe("the region beside a question or a plan", () => {
  it("names the question, then clears when it is answered", () => {
    const question = promptQuestion(
      [
        {
          callId: "call-2",
          turnId: "turn-1",
          askedAt: "2026-09-23T10:00:00Z",
          questions: [
            {
              id: "q1",
              header: "Database",
              question: "Which database should I use?",
              options: [],
              questionType: "single_select",
              allowFreeForm: true,
            },
          ],
        },
      ],
      [],
    );
    const { rerender } = render(<ChatPromptAnnouncer question={null} />);
    const region = screen.getByTestId("chat-prompt-announcer");
    expect(region).toHaveTextContent("");

    rerender(<ChatPromptAnnouncer question={question} />);
    expect(region).toHaveTextContent(
      "Waiting for your answer: Which database should I use?",
    );

    rerender(<ChatPromptAnnouncer question={null} />);
    expect(region).toHaveTextContent("");
  });

  it("names a plan by its title", () => {
    render(
      <ChatPromptAnnouncer
        question={promptQuestion(
          [],
          [
            {
              callId: "call-3",
              turnId: "turn-1",
              title: "Move billing to the new API",
              plan: "1. Read the old client",
              proposedAt: "2026-09-23T10:00:00Z",
            },
          ],
        )}
      />,
    );
    expect(screen.getByTestId("chat-prompt-announcer")).toHaveTextContent(
      "Waiting for your review: Move billing to the new API",
    );
  });
});
