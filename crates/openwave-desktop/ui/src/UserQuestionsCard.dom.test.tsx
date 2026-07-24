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
      allowFreeForm: false,
    },
    {
      id: "note",
      header: "Note",
      question: "Anything else?",
      options: [],
      allowFreeForm: true,
    },
  ],
};

describe("UserQuestionsCard", () => {
  it("submits every exact answer without creating chat text", async () => {
    const user = userEvent.setup();
    const onAnswer = vi.fn();
    render(
      <UserQuestionsCard
        request={request}
        working={false}
        error={undefined}
        onAnswer={onAnswer}
        onCancel={vi.fn()}
      />,
    );
    const submit = screen.getByRole("button", { name: "Continue" });
    expect(submit).toBeDisabled();
    await user.click(screen.getByLabelText(/Staging/));
    await user.type(screen.getByRole("textbox"), "Keep it reversible.");
    expect(submit).toBeEnabled();
    await user.click(submit);
    expect(onAnswer).toHaveBeenCalledWith([
      { questionId: "target", optionId: "staging" },
      { questionId: "note", freeForm: "Keep it reversible." },
    ]);
  });

  it("keeps cancellation explicit and disables mutation while sending", async () => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    render(
      <UserQuestionsCard
        request={request}
        working
        error="Retry safely"
        onAnswer={vi.fn()}
        onCancel={onCancel}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("Retry safely");
    const cancel = screen.getByRole("button", { name: "Cancel turn" });
    expect(cancel).toBeDisabled();
    await user.click(cancel);
    expect(onCancel).not.toHaveBeenCalled();
  });
});
