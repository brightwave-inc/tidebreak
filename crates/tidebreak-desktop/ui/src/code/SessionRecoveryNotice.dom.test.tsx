// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { Attention, FenceReason } from "../api/types";
import { SessionLifecycleIndicator } from "./SessionLifecycleIndicator";
import { SessionRecoveryNotice } from "./SessionRecoveryNotice";
import { RECOVERY_NOTICE_DELAY_MS } from "./useRecoveryDelay";

const recovering: Attention = {
  state: { type: "fenced", reason: { type: "orphan_alive" } },
  source: "lifecycle",
};
const blocked: Attention = {
  state: {
    type: "needs_you",
    prompt: "The engine did not stop. Retry recovery.",
    source: "lifecycle",
  },
  source: "lifecycle",
};
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it("keeps brief recovery quiet and cancels its timer after success", () => {
  vi.useFakeTimers();
  const { container, rerender } = render(
    <SessionRecoveryNotice
      lifecycle="fenced"
      attention={recovering}
      onRetry={vi.fn()}
    />,
  );
  act(() => vi.advanceTimersByTime(RECOVERY_NOTICE_DELAY_MS - 1));
  expect(container).toBeEmptyDOMElement();
  rerender(
    <SessionRecoveryNotice
      lifecycle="idle"
      attention={{ state: { type: "idle" }, source: "lifecycle" }}
      onRetry={vi.fn()}
    />,
  );
  act(() => vi.advanceTimersByTime(RECOVERY_NOTICE_DELAY_MS));
  expect(container).toBeEmptyDOMElement();
});

it("shows delayed progress, then a concrete failure with a single retry action", () => {
  vi.useFakeTimers();
  const retry = vi.fn();
  const { rerender, container } = render(
    <SessionRecoveryNotice
      lifecycle="fenced"
      attention={recovering}
      onRetry={retry}
    />,
  );
  act(() => vi.advanceTimersByTime(RECOVERY_NOTICE_DELAY_MS));
  expect(screen.getByText("Reconnecting…")).toBeInTheDocument();
  expect(screen.queryByRole("button")).toBeNull();
  rerender(
    <SessionRecoveryNotice
      lifecycle="fenced"
      attention={blocked}
      reason={{ type: "orphan_alive" }}
      onRetry={retry}
    />,
  );
  expect(
    screen.getByText(
      blocked.state.type === "needs_you" ? blocked.state.prompt : "",
    ),
  ).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Retry recovery" }));
  expect(retry).toHaveBeenCalledOnce();
  rerender(
    <SessionRecoveryNotice
      lifecycle="fenced"
      attention={blocked}
      reason={{ type: "orphan_alive" }}
      retrying
      onRetry={retry}
    />,
  );
  expect(screen.getByRole("button", { name: "Retrying…" })).toBeDisabled();
  rerender(
    <SessionRecoveryNotice
      lifecycle="idle"
      attention={{ state: { type: "idle" }, source: "lifecycle" }}
      onRetry={retry}
    />,
  );
  expect(container).toBeEmptyDOMElement();
});

it.each<FenceReason>([
  { type: "probe_ambiguous", detail: "Process identity changed" },
  { type: "repeated_turn_failures", count: 3, detail: "Authentication failed" },
])("offers explicit retry after the cause is corrected for $type", (reason) => {
  render(
    <SessionRecoveryNotice
      lifecycle="fenced"
      attention={blocked}
      reason={reason}
      onRetry={vi.fn()}
    />,
  );
  expect(screen.getByRole("button", { name: "Retry recovery" })).toBeEnabled();
  expect(screen.getByRole("status")).toHaveTextContent(
    "The engine did not stop",
  );
});

it("requires explicit consent before continuing without final output", async () => {
  const retry = vi.fn();
  render(
    <SessionRecoveryNotice
      lifecycle="fenced"
      attention={blocked}
      reason={{
        type: "terminal_flush_missing",
        detail: "Final output missing",
      }}
      onRetry={retry}
    />,
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Continue with saved transcript" }),
  );
  expect(retry).not.toHaveBeenCalled();
  expect(screen.getByRole("alertdialog")).toHaveTextContent(
    "The interrupted turn will not run again",
  );
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await act(async () => {});
  expect(retry).not.toHaveBeenCalled();
  fireEvent.click(
    screen.getByRole("button", { name: "Continue with saved transcript" }),
  );
  const buttons = screen.getAllByRole("button", {
    name: "Continue with saved transcript",
  });
  fireEvent.click(buttons[buttons.length - 1]);
  await act(async () => {});
  expect(retry).toHaveBeenCalledOnce();
});

it("keeps the header quiet briefly, then changes immediately from recovery to a blocker", () => {
  vi.useFakeTimers();
  const { container, rerender } = render(
    <SessionLifecycleIndicator
      lifecycle="fenced"
      attention={recovering}
      harness="codex"
      unrecognizedEventCount={0}
    />,
  );
  expect(container).toBeEmptyDOMElement();
  act(() => vi.advanceTimersByTime(RECOVERY_NOTICE_DELAY_MS));
  expect(screen.getByText("Reconnecting…")).toBeInTheDocument();
  rerender(
    <SessionLifecycleIndicator
      lifecycle="fenced"
      attention={blocked}
      harness="codex"
      unrecognizedEventCount={0}
    />,
  );
  expect(screen.getByText("Needs you")).toBeInTheDocument();
  expect(screen.queryByText("Reconnecting…")).toBeNull();
});

it("requires consent for recovery when an older server omits the cause", () => {
  const retry = vi.fn();
  render(
    <SessionRecoveryNotice
      lifecycle="fenced"
      attention={blocked}
      onRetry={retry}
    />,
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Continue with saved transcript" }),
  );
  expect(screen.getByRole("alertdialog")).toBeInTheDocument();
  expect(retry).not.toHaveBeenCalled();
});
