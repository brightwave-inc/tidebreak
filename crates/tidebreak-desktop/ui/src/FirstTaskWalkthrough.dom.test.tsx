// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  FIRST_TASK_WALKTHROUGH_KEY,
  FirstTaskWalkthrough,
  shouldOfferFirstTaskWalkthrough,
  useFirstTaskGuide,
} from "./FirstTaskWalkthrough";

function Targets() {
  return (
    <>
      <button data-first-task-target="model">Model</button>
      <div data-first-task-target="model-menu">Model menu</div>
      <button data-first-task-target="tools">Tools</button>
      <div data-first-task-target="tools-menu">Tools menu</div>
      <button data-first-task-target="permissions">Ask</button>
      <div data-first-task-target="permissions-menu">Permissions menu</div>
      <div data-first-task-target="starters">Starters</div>
      <textarea data-composer-input aria-label="Message" />
    </>
  );
}

function WalkthroughHarness({
  onClose,
}: {
  onClose: (outcome: "completed" | "skipped") => void;
}) {
  const [open, setOpen] = useState(true);
  return (
    <>
      <Targets />
      <FirstTaskWalkthrough
        open={open}
        onClose={(outcome) => {
          onClose(outcome);
          setOpen(false);
        }}
      />
    </>
  );
}

beforeEach(() => {
  window.localStorage.clear();
  useFirstTaskGuide.getState().setSurface(null);
});
afterEach(() => {
  cleanup();
  useFirstTaskGuide.getState().setSurface(null);
});

describe("FirstTaskWalkthrough", () => {
  it("walks the setup controls, opens each surface, remembers completion, and returns focus", async () => {
    const onClose = vi.fn();
    render(<WalkthroughHarness onClose={onClose} />);

    expect(
      screen.getByRole("heading", { name: "Choose a model" }),
    ).toBeInTheDocument();
    expect(useFirstTaskGuide.getState().surface).toBe("model");
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(
      screen.getByRole("heading", { name: "Set internet access" }),
    ).toBeInTheDocument();
    expect(useFirstTaskGuide.getState().surface).toBe("tools");
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(
      screen.getByRole("heading", { name: "Choose a permission level" }),
    ).toBeInTheDocument();
    expect(useFirstTaskGuide.getState().surface).toBe("permissions");
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(
      screen.getByRole("heading", { name: "Add attachments" }),
    ).toBeInTheDocument();
    expect(useFirstTaskGuide.getState().surface).toBe("tools");
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(
      screen.getByRole("heading", { name: "Start a real task" }),
    ).toBeInTheDocument();
    expect(useFirstTaskGuide.getState().surface).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Done" }));

    expect(onClose).toHaveBeenCalledWith("completed");
    expect(window.localStorage.getItem(FIRST_TASK_WALKTHROUGH_KEY)).toBe(
      "completed",
    );
    expect(shouldOfferFirstTaskWalkthrough()).toBe(false);
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Message" })).toHaveFocus(),
    );
  });

  it("treats Escape as an intentional skip", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<WalkthroughHarness onClose={onClose} />);

    await user.keyboard("{Escape}");

    expect(onClose).toHaveBeenCalledWith("skipped");
    expect(window.localStorage.getItem(FIRST_TASK_WALKTHROUGH_KEY)).toBe(
      "skipped",
    );
  });

  it("keeps keyboard focus inside the walkthrough through layout changes", async () => {
    const user = userEvent.setup();
    render(
      <>
        <Targets />
        <FirstTaskWalkthrough open onClose={vi.fn()} />
      </>,
    );

    const next = screen.getByRole("button", { name: "Next" });
    next.focus();
    fireEvent(window, new Event("resize"));
    expect(next).toHaveFocus();

    await user.tab();
    expect(screen.getByRole("button", { name: "Skip setup" })).toHaveFocus();
  });

  it("does not turn a click on the highlighted control into a permanent skip", async () => {
    const onClose = vi.fn();
    render(<WalkthroughHarness onClose={onClose} />);
    await waitFor(() => expect(screen.getByRole("dialog")).toBeInTheDocument());

    const target = screen.getByText("Model");
    fireEvent.pointerDown(target);
    fireEvent.mouseDown(target);
    fireEvent.pointerUp(target);
    fireEvent.mouseUp(target);
    fireEvent.click(target);

    expect(onClose).not.toHaveBeenCalled();
    expect(window.localStorage.getItem(FIRST_TASK_WALKTHROUGH_KEY)).toBeNull();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.getByText("1 of 5").closest("[aria-live]")).toHaveAttribute(
      "aria-live",
      "polite",
    );
  });
});
