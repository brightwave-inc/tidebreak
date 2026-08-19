// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useEffect, useState } from "react";
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
      <div data-first-task-target="model-menu">Model menu</div>
      <div data-first-task-target="model-choice">Selected model</div>
      <div data-first-task-target="tools-menu">Tools menu</div>
      <div data-first-task-target="network">Network</div>
      <div data-first-task-target="attach-files">Attach files</div>
      <div data-first-task-target="permissions-menu">Permissions menu</div>
      <div data-first-task-target="permissions-ask">Ask mode</div>
      <div data-first-task-target="starters">Starters</div>
      <div data-first-task-target="starter-choice">First starter</div>
      <textarea data-composer-input aria-label="Message" />
    </>
  );
}

function mockTargetRect(
  name: string,
  rect: Pick<DOMRect, "top" | "left" | "width" | "height">,
): void {
  const element = document.querySelector(`[data-first-task-target="${name}"]`);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`missing first-task target ${name}`);
  }
  vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
    x: rect.left,
    y: rect.top,
    top: rect.top,
    left: rect.left,
    right: rect.left + rect.width,
    bottom: rect.top + rect.height,
    width: rect.width,
    height: rect.height,
    toJSON: () => ({}),
  });
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
    expect(screen.getByRole("dialog")).toContainElement(
      document.activeElement as HTMLElement | null,
    );
  });

  it("does not turn a click on the highlighted control into a permanent skip", async () => {
    const onClose = vi.fn();
    render(<WalkthroughHarness onClose={onClose} />);
    await waitFor(() => expect(screen.getByRole("dialog")).toBeInTheDocument());

    const target = screen.getByText("Selected model");
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

  it("upgrades the spotlight from the open menu to the specific row", async () => {
    function LateChoice() {
      const [ready, setReady] = useState(false);
      useEffect(() => {
        const frame = window.requestAnimationFrame(() => setReady(true));
        return () => window.cancelAnimationFrame(frame);
      }, []);
      return (
        <>
          <div data-first-task-target="model-menu">Model menu</div>
          {ready && <div data-first-task-target="model-choice">Selected model</div>}
          <textarea data-composer-input aria-label="Message" />
        </>
      );
    }

    render(
      <>
        <LateChoice />
        <FirstTaskWalkthrough open onClose={vi.fn()} />
      </>,
    );

    await waitFor(() =>
      expect(screen.getByText("Selected model")).toBeInTheDocument(),
    );
    mockTargetRect("model-menu", { top: 40, left: 20, width: 320, height: 280 });
    mockTargetRect("model-choice", { top: 80, left: 28, width: 240, height: 36 });
    fireEvent(window, new Event("resize"));

    await waitFor(() => {
      const ring = document.querySelector("[data-first-task-ring]");
      expect(ring).toHaveStyle({ width: "248px", height: "44px" });
    });
  });

  it("spotlights the specific box that opened, not the trigger or the whole menu", async () => {
    render(<WalkthroughHarness onClose={vi.fn()} />);
    mockTargetRect("model-menu", { top: 40, left: 20, width: 320, height: 280 });
    mockTargetRect("model-choice", { top: 80, left: 28, width: 240, height: 36 });
    fireEvent(window, new Event("resize"));

    await waitFor(() => {
      const ring = document.querySelector("[data-first-task-ring]");
      expect(ring).toHaveStyle({ width: "248px", height: "44px" });
    });
  });
});
