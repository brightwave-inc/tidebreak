// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { PlanDecisionResultCard } from "./PlanDecisionResultCard";
import { UserQuestionsResultCard } from "./UserQuestionsResultCard";

afterEach(cleanup);

/**
 * The turn stopped to ask, so the settled transcript has to say what it was
 * told — including the questions that were passed over, which read as an answer
 * of their own.
 */
it("recaps chosen labels and names what was skipped", () => {
  render(
    <UserQuestionsResultCard
      answers={[
        {
          question: "Where should I deploy?",
          selected: ["Staging", "Production"],
          customAnswer: "Start with a canary.",
        },
        { question: "Anything else?", selected: [], customAnswer: null },
      ]}
      additionalContext="Keep the rollout reversible."
    />,
  );
  expect(screen.getByText("1. Where should I deploy?")).toBeInTheDocument();
  expect(
    screen.getByText("Staging, Production, Start with a canary."),
  ).toBeInTheDocument();
  expect(screen.getByText("Skipped")).toBeInTheDocument();
  expect(screen.getByText("Keep the rollout reversible.")).toBeInTheDocument();
});

it("says so when every question was passed over", () => {
  render(
    <UserQuestionsResultCard
      answers={[
        { question: "Anything else?", selected: [], customAnswer: null },
      ]}
      additionalContext={null}
    />,
  );
  expect(screen.getByText("All questions were skipped.")).toBeInTheDocument();
});

/** A revised plan carries the feedback that sent it back; an accepted one has none. */
it("shows the revision feedback only on a rejected plan", () => {
  const { rerender } = render(
    <PlanDecisionResultCard
      title="Add health checks"
      plan="1. Add a `/healthz` route."
      accepted={false}
      feedback="Split step 2 into its own slice."
    />,
  );
  expect(screen.getByText("Revised")).toBeInTheDocument();
  expect(
    screen.getByText("Split step 2 into its own slice."),
  ).toBeInTheDocument();

  rerender(
    <PlanDecisionResultCard
      title="Add health checks"
      plan="1. Add a `/healthz` route."
      accepted
      feedback={null}
    />,
  );
  expect(screen.getByText("Accepted")).toBeInTheDocument();
  expect(screen.queryByText("Revision feedback")).not.toBeInTheDocument();
});
