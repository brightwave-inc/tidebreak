// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { TaskPlan } from "./api";
import { TaskPlanCard } from "./TaskPlanCard";

afterEach(cleanup);

const PLAN: TaskPlan = {
  turn_id: "turn-1",
  updated_at: "2026-01-01T00:00:00Z",
  steps: [
    { content: "Read the spec", status: "completed" },
    { content: "Draft the change", status: "in_progress" },
    { content: "Run the tests", status: "pending" },
  ],
};

describe("TaskPlanCard", () => {
  it("stops claiming work is under way once the plan's turn has ended", () => {
    const { container, rerender } = render(
      <TaskPlanCard plan={PLAN} live={true} />,
    );

    // While the turn runs the plan is open and the current step uses a comet.
    expect(screen.getByRole("button", { name: /task plan/i })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    expect(screen.getByText("Draft the change")).toBeInTheDocument();
    expect(
      container.querySelector("[data-loader-variant='comet']"),
    ).not.toBeNull();

    rerender(<TaskPlanCard plan={PLAN} live={false} />);

    // The plan stays as history, but nothing in it still animates.
    expect(screen.getByRole("button", { name: /task plan/i })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(screen.getByText("1/3")).toBeInTheDocument();
    expect(container.querySelector("[data-loader-variant='comet']")).toBeNull();
  });
});
