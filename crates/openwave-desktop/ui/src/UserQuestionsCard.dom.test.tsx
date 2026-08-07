// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { UserQuestionsCard } from "./UserQuestionsCard";

afterEach(cleanup);

const request = {
  callId: "call-1",
  turnId: "turn-1",
  askedAt: "2026-07-24T12:00:00Z",
  questions: [
    {
      id: "target",
      header: "Target",
      question: "Where should I deploy?",
      options: [
        {
          id: "staging",
          label: "Staging",
          description: "Deploy for internal verification.",
        },
        {
          id: "production",
          label: "Production",
          description: "Deploy to customers.",
        },
      ],
      questionType: "multi_select" as const,
      allowFreeForm: true,
    },
    {
      id: "note",
      header: "Note",
      question: "Anything else?",
      options: [],
      questionType: "single_select" as const,
      allowFreeForm: true,
    },
  ],
};

describe("UserQuestionsCard", () => {
  it("submits multi-select, partial answers, and additional context together", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    render(
      <UserQuestionsCard
        request={request}
        working={false}
        error={undefined}
        onAnswer={onAnswer}
      />,
    );
    expect(
      screen.queryByRole("button", { name: "Continue" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByLabelText(/Staging/));
    await user.click(screen.getByLabelText(/Production/));
    await user.type(
      screen.getByRole("textbox", { name: "Other answer" }),
      "Start with a canary.",
    );
    await user.click(screen.getByRole("button", { name: "Next" }));
    // The second page is where the last question lives; context is a page of
    // its own behind "Continue and add context".
    await user.click(
      screen.getByRole("button", { name: "Continue and add context" }),
    );
    await user.type(
      screen.getByRole("textbox", { name: "Additional context" }),
      "Keep it reversible.",
    );
    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect(onAnswer).toHaveBeenCalledWith(
      [
        {
          questionId: "target",
          selectedOptionIds: ["staging", "production"],
          customAnswer: "Start with a canary.",
        },
      ],
      "Keep it reversible.",
    );
  });

  /**
   * Paging back has to keep what was already chosen: the answers are collected
   * across pages and only sent once, so a lost draft is a silently wrong answer.
   */
  it("keeps answers when paging back and forward", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    render(
      <UserQuestionsCard
        request={request}
        working={false}
        error={undefined}
        onAnswer={onAnswer}
      />,
    );
    await user.click(screen.getByLabelText(/Staging/));
    await user.click(screen.getByRole("button", { name: "Next" }));
    await user.click(screen.getByRole("button", { name: "Back" }));
    expect(screen.getByLabelText(/Staging/)).toBeChecked();
    await user.click(screen.getByRole("button", { name: "Go to question 2" }));
    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect(onAnswer).toHaveBeenCalledWith(
      [{ questionId: "target", selectedOptionIds: ["staging"] }],
      undefined,
    );
  });

  it("skips all questions from the footer menu without cancelling the turn", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    render(
      <UserQuestionsCard
        request={request}
        working={false}
        error={undefined}
        onAnswer={onAnswer}
      />,
    );

    await user.click(screen.getByRole("button", { name: /Skip all/ }));
    await user.click(
      await screen.findByRole("menuitem", { name: "Skip these questions" }),
    );
    expect(onAnswer).toHaveBeenCalledWith([]);
  });

  it("skips all questions without cancelling the turn", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    render(
      <UserQuestionsCard
        request={request}
        working={false}
        error={undefined}
        onAnswer={onAnswer}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Skip questions" }));
    expect(onAnswer).toHaveBeenCalledWith([]);
  });

  it("disables submission while sending", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    render(
      <UserQuestionsCard
        request={request}
        working
        error="Retry safely"
        onAnswer={onAnswer}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("Retry safely");
    const skip = screen.getByRole("button", { name: "Skip questions" });
    expect(skip).toBeDisabled();
    await user.click(skip);
    expect(onAnswer).not.toHaveBeenCalled();
  });
});
