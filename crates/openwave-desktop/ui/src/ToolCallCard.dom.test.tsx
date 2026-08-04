// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useChatSessionStore } from "./ChatSessionStore";
import { ToolCommandCard } from "./ToolCallCard";

const preview = {
  tool: "exec" as const,
  command: "python3",
  args: ["render.py"],
  cwd: ".",
};

afterEach(() => {
  cleanup();
  useChatSessionStore.getState().reset();
});

describe("ToolCommandCard sandbox preparation", () => {
  it("shows a first-run image pull on the command that is waiting for it", () => {
    useChatSessionStore
      .getState()
      .update((session) => ({ ...session, sandboxPreparing: true }));

    render(
      <ToolCommandCard
        name="exec"
        status="running"
        preview={preview}
        result={null}
      />,
    );

    expect(
      screen.getByText(/Preparing the sandbox image \(first run only\)/),
    ).toBeTruthy();
    expect(screen.getByText("Preparing sandbox\u2026")).toBeTruthy();
  });

  it("keeps the notice off a command that is no longer waiting", () => {
    useChatSessionStore
      .getState()
      .update((session) => ({ ...session, sandboxPreparing: true }));

    render(
      <ToolCommandCard
        name="exec"
        status="completed"
        preview={preview}
        result={null}
      />,
    );

    expect(screen.queryByText(/Preparing the sandbox image/)).toBeNull();
  });
});
