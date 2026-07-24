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
  canRemember: true,
};

const ONCE = "1.Yes, allow it once";
const REMEMBER = "2.Yes, and don't ask again in this chat";

afterEach(cleanup);

describe("approval card interactions", () => {
  it("propagates each decision with its remember flag", async () => {
    const user = userEvent.setup();
    const onApproval = vi.fn();
    render(
      <MessageBubble message={approval} busy={false} onApproval={onApproval} />,
    );

    const options = screen.getAllByRole("option");
    expect(options.map((option) => option.textContent)).toEqual([
      ONCE,
      REMEMBER,
      "3.No, don't allow this",
    ]);

    await user.click(options[0]!);
    expect(onApproval).toHaveBeenLastCalledWith("call-1", "approve", false);

    await user.click(options[1]!);
    expect(onApproval).toHaveBeenLastCalledWith("call-1", "approve", true);

    await user.click(options[2]!);
    expect(onApproval).toHaveBeenLastCalledWith("call-1", "reject", false);
  });

  it("starts on the narrowest grant so a stray Enter cannot widen scope", () => {
    render(
      <MessageBubble message={approval} busy={false} onApproval={vi.fn()} />,
    );

    const options = screen.getAllByRole("option");
    expect(options[0]).toHaveAttribute("aria-selected", "true");
    expect(options[0]?.textContent).toBe(ONCE);
  });

  it("submits the highlighted option from the keyboard", async () => {
    const user = userEvent.setup();
    const onApproval = vi.fn();
    render(
      <MessageBubble message={approval} busy={false} onApproval={onApproval} />,
    );

    const options = screen.getAllByRole("option");
    options[0]!.focus();
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onApproval).toHaveBeenLastCalledWith("call-1", "approve", true);

    await user.keyboard("3{Enter}");
    expect(onApproval).toHaveBeenLastCalledWith("call-1", "reject", false);
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

    for (const option of screen.getAllByRole("option")) {
      await user.click(option);
    }
    expect(screen.getByRole("button", { name: "Submit" })).toBeDisabled();
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
    await user.click(screen.getAllByRole("option")[0]!);
    expect(onApproval).toHaveBeenCalledWith("call-1", "approve", false);
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
      screen.getAllByRole("option").map((option) => option.textContent),
    ).toEqual(["1.No, don't allow this"]);
  });

  it("offers one-shot approval but no remembered grant for MCP", () => {
    render(
      <MessageBubble
        message={{ ...approval, canRemember: false }}
        busy={false}
        onApproval={() => undefined}
      />,
    );

    expect(
      screen.getAllByRole("option").map((option) => option.textContent),
    ).toEqual([ONCE, "2.No, don't allow this"]);
  });

  it("shows the command an exec approval is granting", () => {
    render(
      <MessageBubble
        message={{
          ...approval,
          summary:
            "Allow OpenWave to run a command that leaves the chat workspace and may reach the network?",
          preview: {
            tool: "exec",
            command: "cargo",
            args: ["test", "--workspace"],
            cwd: "checkout",
          },
        }}
        busy={false}
        onApproval={() => undefined}
      />,
    );

    expect(screen.getByText(/cargo test --workspace/)).toBeInTheDocument();
    expect(
      screen.getByText(/# working directory: checkout/),
    ).toBeInTheDocument();
  });
});

describe("row memoization", () => {
  it("MessageBubble is a memoized component", () => {
    expect(
      (MessageBubble as unknown as { $$typeof: symbol }).$$typeof,
    ).toBe(Symbol.for("react.memo"));
  });
});
