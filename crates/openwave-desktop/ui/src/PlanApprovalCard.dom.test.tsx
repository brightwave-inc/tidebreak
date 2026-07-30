// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PlanApprovalCard } from "./PlanApprovalCard";

afterEach(cleanup);

const request = {
  callId: "call-1",
  turnId: "turn-1",
  title: "Add health checks",
  plan: "## Steps\n1. Add a `/healthz` route.\n2. Cover it with one lifecycle test.",
  proposedAt: "2026-07-30T12:00:00Z",
};

describe("PlanApprovalCard", () => {
  it("approves with one click and no feedback payload", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
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
    await user.click(screen.getByRole("button", { name: /Approve and run/ }));
    expect(onDecide).toHaveBeenCalledWith({ decision: "accept" });
  });

  it("sends the plan back with the exact typed feedback", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(
      <PlanApprovalCard
        request={request}
        working={false}
        error={undefined}
        onDecide={onDecide}
        onCancel={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Request changes" }));
    await user.type(
      screen.getByLabelText("What should change"),
      "Split step 2 into its own slice.",
    );
    await user.click(
      screen.getByRole("button", { name: "Send back for changes" }),
    );
    expect(onDecide).toHaveBeenCalledWith({
      decision: "reject",
      feedback: "Split step 2 into its own slice.",
    });
  });
});
