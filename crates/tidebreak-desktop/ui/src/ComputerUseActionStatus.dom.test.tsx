// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
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
  startedAtMillis: 1000,
  visibleUntilMillis: 3000,
};
afterEach(cleanup);

describe("computer-use action status", () => {
  it("describes pending background and foreground calls as requests", () => {
    const { rerender } = render(
      <ComputerUseActionStatus action={action} now={2000} />,
    );
    expect(screen.getByText("Requesting a click")).toBeTruthy();
    expect(screen.getByText("Background request")).toBeTruthy();
    rerender(
      <ComputerUseActionStatus
        action={{ ...action, executionMode: "foreground" }}
        now={2000}
      />,
    );
    expect(screen.getByText("Requesting a click")).toBeTruthy();
    expect(screen.getByText("Foreground request")).toBeTruthy();
    expect(screen.queryByText("Using the foreground")).toBeNull();
  });

  it("reports completion and foreground refusal as distinct outcomes", () => {
    const { rerender } = render(
      <ComputerUseActionStatus
        action={{ ...action, phase: "completed" }}
        now={2000}
      />,
    );
    expect(screen.getByText("Action completed")).toBeTruthy();
    expect(screen.getByText("Ran in the background")).toBeTruthy();
    rerender(
      <ComputerUseActionStatus
        action={{ ...action, phase: "foreground_required" }}
        now={2000}
      />,
    );
    expect(screen.getByText("Needs foreground access")).toBeTruthy();
    expect(
      screen.getByText("Foreground control needs your approval"),
    ).toBeTruthy();
  });
});
