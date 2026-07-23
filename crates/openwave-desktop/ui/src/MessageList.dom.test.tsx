// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MessageBubble, type ChatMessage } from "./MessageList";

const approval: ChatMessage = {
  id: "approval-1",
  role: "approval",
  callId: "call-1",
  summary: "Search a site",
  canApprove: true,
};

afterEach(cleanup);

describe("approval card interactions", () => {
  it("propagates each decision with its remember flag", async () => {
    const user = userEvent.setup();
    const onApproval = vi.fn();
    render(
      <MessageBubble message={approval} busy={false} onApproval={onApproval} />,
    );

    await user.click(screen.getByRole("button", { name: "Approve once" }));
    expect(onApproval).toHaveBeenLastCalledWith("call-1", "approve");

    await user.click(
      screen.getByRole("button", { name: "Allow for this chat" }),
    );
    expect(onApproval).toHaveBeenLastCalledWith("call-1", "approve", true);

    await user.click(screen.getByRole("button", { name: "Reject" }));
    expect(onApproval).toHaveBeenLastCalledWith("call-1", "reject");
  });

  it("blocks every decision while one is in flight", async () => {
    const user = userEvent.setup();
    const onApproval = vi.fn();
    render(
      <MessageBubble
        message={approval}
        busy={false}
        onApproval={onApproval}
        approvalState={{
          decidingApprovalCalls: new Set(["call-1"]),
          approvalErrors: {},
        }}
      />,
    );

    for (const name of ["Approve once", "Allow for this chat", "Reject"]) {
      const button = screen.getByRole("button", { name });
      expect(button).toBeDisabled();
      await user.click(button);
    }
    expect(onApproval).not.toHaveBeenCalled();
  });

  it("announces a failed decision and stays actionable for retry", async () => {
    const user = userEvent.setup();
    const onApproval = vi.fn();
    render(
      <MessageBubble
        message={approval}
        busy={false}
        onApproval={onApproval}
        approvalState={{
          decidingApprovalCalls: new Set(),
          approvalErrors: {
            "call-1": "Could not send your decision: Error: 500",
          },
        }}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Could not send your decision",
    );
    await user.click(screen.getByRole("button", { name: "Approve once" }));
    expect(onApproval).toHaveBeenCalledWith("call-1", "approve");
  });

  it("offers only rejection when the action kind is not approvable", () => {
    render(
      <MessageBubble
        message={{ ...approval, canApprove: false }}
        busy={false}
        onApproval={() => undefined}
      />,
    );

    expect(
      screen.queryByRole("button", { name: "Approve once" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Allow for this chat" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject" })).toBeInTheDocument();
  });
});

describe("row memoization", () => {
  it("MessageBubble is a memoized component", () => {
    expect(
      (MessageBubble as unknown as { $$typeof: symbol }).$$typeof,
    ).toBe(Symbol.for("react.memo"));
  });
});
