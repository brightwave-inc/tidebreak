// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PlanApprovalCard } from "./PlanApprovalCard";
import { usePlanComments } from "./PlanComments";

afterEach(cleanup);
beforeEach(() => {
  window.localStorage.clear();
  usePlanComments.setState({ byCall: {} });
});

const request = {
  callId: "call-1",
  turnId: "turn-1",
  title: "Add health checks",
  plan: "## Steps\n\n1. Add a `/healthz` route.\n\n2. Cover it with one lifecycle test.",
  proposedAt: "2026-07-30T12:00:00Z",
};

describe("PlanApprovalCard", () => {
  it("approves with one click and no feedback payload", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn().mockResolvedValue(true);
    render(
      <PlanApprovalCard
        request={request}
        working={false}
        error={undefined}
        onDecide={onDecide}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByText("Add health checks")).toBeInTheDocument();
    expect(screen.getByText(/lifecycle test/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Execute plan/ }));
    expect(onDecide).toHaveBeenCalledWith({ decision: "accept" });
  });

  /**
   * The revision path is per-block comments now, and the server still takes one
   * feedback string — this pins the join, which is the only place the two
   * shapes meet.
   */
  it("sends block comments back as quoted feedback", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn().mockResolvedValue(true);
    render(
      <PlanApprovalCard
        request={request}
        working={false}
        error={undefined}
        onDecide={onDecide}
        onCancel={vi.fn()}
      />,
    );
    const [firstStep] = screen.getAllByRole("button", { name: "Add comment" });
    await user.click(firstStep!);
    await user.type(
      screen.getByLabelText("What should change"),
      "Split this into its own slice.",
    );
    await user.click(screen.getByRole("button", { name: "Save" }));

    expect(screen.getByText("1 edit added")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Update plan/ }));
    expect(onDecide).toHaveBeenCalledWith({
      decision: "reject",
      feedback: "> Add a `/healthz` route.\n\nSplit this into its own slice.",
    });
  });

  /**
   * A refused send used to take the comments with it: the card cleared them
   * before it knew whether the decision had landed, and nothing anywhere could
   * give them back. The reader would have to read the plan and write every
   * note again.
   */
  it("keeps block comments when the decision does not reach the server", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn().mockResolvedValue(false);
    render(
      <PlanApprovalCard
        request={request}
        working={false}
        error="Could not send that decision."
        onDecide={onDecide}
        onCancel={vi.fn()}
      />,
    );
    const [firstStep] = screen.getAllByRole("button", { name: "Add comment" });
    await user.click(firstStep!);
    await user.type(
      screen.getByLabelText("What should change"),
      "Split this into its own slice.",
    );
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(screen.getByRole("button", { name: /Update plan/ }));

    expect(onDecide).toHaveBeenCalled();
    expect(screen.getByText("1 edit added")).toBeInTheDocument();
    expect(usePlanComments.getState().byCall["call-1"]).toHaveLength(1);
  });

  it("drops pending edits when they are cancelled", async () => {
    const user = userEvent.setup();
    render(
      <PlanApprovalCard
        request={request}
        working={false}
        error={undefined}
        onDecide={vi.fn().mockResolvedValue(true)}
        onCancel={vi.fn()}
      />,
    );
    const [firstStep] = screen.getAllByRole("button", { name: "Add comment" });
    await user.click(firstStep!);
    await user.type(
      screen.getByLabelText("What should change"),
      "Reword this.",
    );
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(screen.getByRole("button", { name: "Cancel edits" }));
    expect(screen.getByText("Hover over plan to edit")).toBeInTheDocument();
  });
});
