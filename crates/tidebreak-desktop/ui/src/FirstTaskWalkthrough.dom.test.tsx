// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

beforeEach(() => window.localStorage.clear());
afterEach(cleanup);

describe("FirstTaskWalkthrough", () => {
  it("walks the four setup controls, remembers completion, and returns focus", async () => {
    const onClose = vi.fn();
    render(
      <>
        <Targets />
        <FirstTaskWalkthrough open onClose={onClose} />
      </>,
    );

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

  it("treats Escape as an intentional skip", () => {
    const onClose = vi.fn();
    render(
      <>
        <Targets />
        <FirstTaskWalkthrough open onClose={onClose} />
      </>,
    );

    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).toHaveBeenCalledWith("skipped");
    expect(window.localStorage.getItem(FIRST_TASK_WALKTHROUGH_KEY)).toBe(
      "skipped",
    );
  });
});
