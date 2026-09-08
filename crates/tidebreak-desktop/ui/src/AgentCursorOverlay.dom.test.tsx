// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import {
  AgentCursorOverlay,
  agentCursorPosition,
  type AgentCursorPreview,
} from "./AgentCursorOverlay";
import { ComputerUseActionStatus } from "./ComputerUseActionStatus";
import type { ComputerUseAction } from "./computerUseAction";

const action: ComputerUseAction = {
  actionId: "action-1",
  sessionId: "session-1",
  source: "browser",
  action: "click",
  phase: "running",
  executionMode: "background",
  coordinateFrame: "viewport",
  browserId: "browser-1",
  workspaceId: "workspace-1",
  documentEpoch: 3,
  point: { x: 400, y: 300 },
  viewport: { width: 800, height: 600 },
  startedAtMillis: 1000,
  visibleUntilMillis: 3000,
};
const preview: AgentCursorPreview = {
  source: "browser",
  sessionId: "session-1",
  browserId: "browser-1",
  workspaceId: "workspace-1",
  documentEpoch: 3,
  coordinateFrame: "viewport",
  width: 800,
  height: 600,
};
afterEach(cleanup);

describe("agent cursor preview", () => {
  it("maps exact viewport coordinates without consuming pointer input", () => {
    const { container } = render(
      <AgentCursorOverlay action={action} preview={preview} now={2000} />,
    );
    const overlay = container.querySelector("[data-agent-cursor-overlay]");
    expect(overlay?.classList.contains("pointer-events-none")).toBe(true);
    expect(overlay?.getAttribute("aria-hidden")).toBe("true");
    expect(agentCursorPosition(action, preview)).toEqual({
      left: "50%",
      top: "50%",
    });
    expect(container.querySelector("[tabindex],button,input")).toBeNull();
  });

  it("hides expired and unexecuted actions", () => {
    const { container, rerender } = render(
      <AgentCursorOverlay action={action} preview={preview} now={3000} />,
    );
    expect(container.firstChild).toBeNull();
    for (const phase of [
      "failed",
      "foreground_required",
      "cancelled",
    ] as const) {
      rerender(
        <AgentCursorOverlay
          action={{ ...action, phase }}
          preview={preview}
          now={2000}
        />,
      );
      expect(container.firstChild).toBeNull();
    }
  });

  it("rejects stale epochs, wrong targets, bounds, and screen coordinates", () => {
    for (const changed of [
      { ...preview, documentEpoch: 4 },
      { ...preview, browserId: "different" },
      { ...preview, width: 1000 },
      { ...preview, sessionId: "different" },
      { ...preview, workspaceId: "different" },
    ])
      expect(agentCursorPosition(action, changed)).toBeNull();
    expect(
      agentCursorPosition({ ...action, coordinateFrame: "screen" }, preview),
    ).toBeNull();
    expect(
      agentCursorPosition({ ...action, point: { x: -1, y: 10 } }, preview),
    ).toBeNull();
    expect(
      agentCursorPosition({ ...action, point: { x: 800, y: 10 } }, preview),
    ).toBeNull();
    expect(
      agentCursorPosition({ ...action, point: { x: NaN, y: 10 } }, preview),
    ).toBeNull();
  });

  it("requires exact capture identity for native preview geometry", () => {
    const native = {
      ...action,
      source: "native" as const,
      coordinateFrame: "window" as const,
      bundleId: "test.fixture",
      windowId: 42,
      captureId: "capture-1",
    };
    const target = {
      ...preview,
      source: "native" as const,
      coordinateFrame: "window" as const,
      bundleId: "test.fixture",
      windowId: 42,
      captureId: "capture-1",
    };
    expect(agentCursorPosition(native, target)).not.toBeNull();
    expect(
      agentCursorPosition(native, { ...target, captureId: undefined }),
    ).toBeNull();
    expect(
      agentCursorPosition(native, { ...target, captureId: "capture-old" }),
    ).toBeNull();
  });

  it("distinguishes background execution from waiting for foreground access", () => {
    const { rerender } = render(
      <ComputerUseActionStatus action={action} now={2000} />,
    );
    expect(screen.getByText("Working in the background")).toBeTruthy();
    rerender(
      <ComputerUseActionStatus
        action={{ ...action, phase: "foreground_required" }}
        now={2000}
      />,
    );
    expect(screen.getByText("Needs foreground access")).toBeTruthy();
    expect(
      screen.getByText("Waiting for permission to use the foreground"),
    ).toBeTruthy();
  });
});
