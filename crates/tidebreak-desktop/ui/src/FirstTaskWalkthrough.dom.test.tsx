// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  FIRST_TASK_WALKTHROUGH_KEY,
  FirstTaskWalkthrough,
  shouldOfferFirstTaskWalkthrough,
} from "./FirstTaskWalkthrough";

function Targets() {
  return (
    <>
      <button data-first-task-target="model">Model</button>
      <button data-first-task-target="tools">Tools</button>
      <button data-first-task-target="permissions">Ask</button>
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

beforeEach(() => window.localStorage.clear());
afterEach(cleanup);

describe("FirstTaskWalkthrough", () => {
  it("walks the four setup controls, remembers completion, and returns focus", async () => {
    const onClose = vi.fn();
    render(<WalkthroughHarness onClose={onClose} />);

    expect(
      screen.getByRole("heading", { name: "Choose a model" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(
      screen.getByRole("heading", { name: "Set internet access" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(
      screen.getByRole("heading", { name: "Choose a permission level" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(
      screen.getByRole("heading", { name: "Add attachments" }),
    ).toBeInTheDocument();
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
    expect(screen.getByRole("dialog")).toContainElement(
      document.activeElement as HTMLElement | null,
    );
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
    expect(screen.getByText("1 of 4").parentElement?.parentElement).toHaveAttribute(
      "aria-live",
      "polite",
    );
  });
});
