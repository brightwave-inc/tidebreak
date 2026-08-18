// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeComposer } from "./CodeComposer";
import { HttpError } from "../api/client";
afterEach(() => {
  cleanup();
});

const QUEUED = {
  kind: "queued" as const,
  queued: { session_id: "sess-1", message: "and run the tests", position: 1 },
};

describe("CodeComposer", () => {
  it("states the session's mode in the composer", () => {
    render(
      <CodeComposer
        running={false}
        permissionMode="ask"
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Permissions: Ask" })).toBeInTheDocument();
  });

  it("queues a follow-up written while a turn is running", async () => {
    const onSend = vi.fn().mockResolvedValue(QUEUED);
    render(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Stop response" })).toBeEnabled();

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "and run the tests" } });
    expect(screen.getByRole("button", { name: "Queue message for after this response" })).toBeEnabled();
    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() => expect(onSend).toHaveBeenCalledWith("and run the tests"));
    expect(box).toHaveValue("");
    expect(
      await screen.findByText("Queued — runs after the current turn."),
    ).toBeInTheDocument();
  });

  it("says the queue is full and keeps the draft", async () => {
    const onSend = vi
      .fn()
      .mockRejectedValue(
        new HttpError(409, "409: a follow-up is already queued", "queue_full"),
      );
    render(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "and push it" } });
    fireEvent.click(
      screen.getByRole("button", { name: "Queue message for after this response" }),
    );

    expect(
      await screen.findByText("A follow-up is already queued. Wait for it to run, or interrupt this turn."),
    ).toBeInTheDocument();
    expect(box).toHaveValue("and push it");
  });

  it("keeps the draft when send is refused", async () => {
    const onSend = vi.fn().mockRejectedValue(new Error("session is fenced"));
    render(
      <CodeComposer
        running={false}
        permissionMode="plan"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );
    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "list the files" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(onSend).toHaveBeenCalled());
    expect(box).toHaveValue("list the files");
  });

  it("changes the selected harness model", async () => {
    const user = userEvent.setup();
    const onModelChange = vi.fn();
    render(
      <CodeComposer
        running={false}
        permissionMode="ask"
        harness="claude_code"
        model="sonnet"
        modelOptions={[
          {
            id: "sonnet",
            label: "Sonnet 4.6",
            source: "Claude Code",
            default: true,
          },
          { id: "opus", label: "Opus 4.6", source: "Claude Code" },
        ]}
        onModelChange={onModelChange}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Model: Sonnet 4.6" }));
    await user.click(screen.getByRole("menuitem", { name: /Opus 4.6/ }));
    expect(onModelChange).toHaveBeenCalledWith("opus");
  });
});
