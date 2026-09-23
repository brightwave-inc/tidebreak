// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { QuitPrompt } from "./QuitPrompt";

afterEach(cleanup);

describe("QuitPrompt", () => {
  it("shows nothing while no quit is under way", () => {
    render(<QuitPrompt prompt={{ phase: "idle" }} onChoose={vi.fn()} />);
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(screen.queryByRole("region", { name: "Quitting" })).toBeNull();
  });

  it("names how many agents are working and offers the three choices", async () => {
    const user = userEvent.setup();
    const onChoose = vi.fn();
    render(
      <QuitPrompt
        prompt={{ phase: "asking", agents: 3, waitingForYou: 0 }}
        onChoose={onChoose}
      />,
    );

    const dialog = screen.getByRole("alertdialog");
    expect(dialog).toHaveTextContent("Quit while 3 agents are working?");
    expect(dialog).not.toHaveTextContent("needs your answer");
    // Cancel is the safe default.
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();

    await user.click(
      screen.getByRole("button", { name: "Quit when they reach a safe point" }),
    );
    await user.click(
      screen.getByRole("button", { name: "Quit and stop them" }),
    );
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onChoose.mock.calls).toEqual([["safe_point"], ["stop"], ["cancel"]]);
  });

  it("speaks of one agent in the singular", () => {
    render(
      <QuitPrompt
        prompt={{ phase: "asking", agents: 1, waitingForYou: 0 }}
        onChoose={vi.fn()}
      />,
    );
    expect(screen.getByRole("alertdialog")).toHaveTextContent(
      "Quit while an agent is working?",
    );
    expect(
      screen.getByRole("button", { name: "Quit when it reaches a safe point" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Quit and stop it" }),
    ).toBeInTheDocument();
  });

  /** An agent parked on an approval never gets to a safe point alone. */
  it("says an agent is waiting for an answer and offers the inbox", async () => {
    const user = userEvent.setup();
    const onChoose = vi.fn();
    const onOpenInbox = vi.fn();
    render(
      <QuitPrompt
        prompt={{ phase: "asking", agents: 2, waitingForYou: 1 }}
        onChoose={onChoose}
        onOpenInbox={onOpenInbox}
      />,
    );
    expect(screen.getByRole("alertdialog")).toHaveTextContent(
      "One of them needs your answer before it can reach a safe point.",
    );

    await user.click(screen.getByRole("button", { name: "Open inbox" }));
    // The inbox is behind the dialog, so going there calls the quit off.
    expect(onChoose).toHaveBeenCalledWith("cancel");
    expect(onOpenInbox).toHaveBeenCalledOnce();
  });

  it("treats Escape as Cancel", async () => {
    const user = userEvent.setup();
    const onChoose = vi.fn();
    render(
      <QuitPrompt
        prompt={{ phase: "asking", agents: 2, waitingForYou: 0 }}
        onChoose={onChoose}
      />,
    );
    await user.keyboard("{Escape}");
    expect(onChoose).toHaveBeenCalledWith("cancel");
  });

  /**
   * The wait leaves the app usable, so the person can answer the approval an
   * agent is parked on: no dialog covers it.
   */
  it("while waiting, shows a bar that counts down and still lets the person stop or cancel", async () => {
    const user = userEvent.setup();
    const onChoose = vi.fn();
    const onOpenInbox = vi.fn();
    render(
      <QuitPrompt
        prompt={{ phase: "waiting", agents: 2, waitingForYou: 1 }}
        onChoose={onChoose}
        onOpenInbox={onOpenInbox}
      />,
    );
    expect(screen.queryByRole("alertdialog")).toBeNull();
    const bar = screen.getByRole("region", { name: "Quitting" });
    expect(bar).toHaveTextContent("Quitting when 2 agents reach a safe point");
    expect(bar).toHaveTextContent("One needs your answer first.");
    expect(
      screen.queryByRole("button", { name: /safe point/ }),
    ).not.toBeInTheDocument();

    // Opening the inbox keeps the quit waiting.
    await user.click(screen.getByRole("button", { name: "Open inbox" }));
    expect(onOpenInbox).toHaveBeenCalledOnce();
    expect(onChoose).not.toHaveBeenCalled();

    await user.click(
      screen.getByRole("button", { name: "Quit and stop them" }),
    );
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onChoose.mock.calls).toEqual([["stop"], ["cancel"]]);
  });

  it("while waiting with no answer owed, says new messages wait", () => {
    render(
      <QuitPrompt
        prompt={{ phase: "waiting", agents: 1, waitingForYou: 0 }}
        onChoose={vi.fn()}
        onOpenInbox={vi.fn()}
      />,
    );
    const bar = screen.getByRole("region", { name: "Quitting" });
    expect(bar).toHaveTextContent(
      "Quitting when the agent reaches a safe point",
    );
    expect(bar).toHaveTextContent(
      "New messages wait until Tidebreak opens again.",
    );
    expect(screen.queryByRole("button", { name: "Open inbox" })).toBeNull();
  });

  it("offers nothing to choose while the agents stop", async () => {
    const user = userEvent.setup();
    const onChoose = vi.fn();
    render(<QuitPrompt prompt={{ phase: "stopping" }} onChoose={onChoose} />);
    expect(screen.getByRole("alertdialog")).toHaveTextContent(
      "Stopping agents",
    );
    expect(screen.queryByRole("button")).toBeNull();
    await user.keyboard("{Escape}");
    expect(onChoose).not.toHaveBeenCalled();
  });

  it("says why a wait for a safe point failed", () => {
    render(
      <QuitPrompt
        prompt={{ phase: "asking", agents: 1, waitingForYou: 0 }}
        error="Chat turns could not be released in time. Try again in a moment."
        onChoose={vi.fn()}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Tidebreak could not reach a safe point. Chat turns could not be released in time.",
    );
  });

  it("holds the buttons while an answer is on its way", () => {
    render(
      <QuitPrompt
        prompt={{ phase: "asking", agents: 2, waitingForYou: 1 }}
        answering
        onChoose={vi.fn()}
        onOpenInbox={vi.fn()}
      />,
    );
    for (const button of screen.getAllByRole("button")) {
      expect(button).toBeDisabled();
    }
  });
});
