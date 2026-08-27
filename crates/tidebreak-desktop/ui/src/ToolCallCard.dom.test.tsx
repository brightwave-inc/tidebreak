// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { useChatSessionStore } from "./ChatSessionStore";
import { ToolCommandCard } from "./ToolCallCard";

const preview = {
  tool: "exec" as const,
  command: "python3",
  args: ["render.py"],
  cwd: ".",
  files: [],
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

describe("ToolCommandCard expansion", () => {
  it("reveals the backend and successful outcome with the command detail", async () => {
    const user = userEvent.setup();
    render(
      <ToolCommandCard
        name="exec"
        status="completed"
        preview={preview}
        result={{
          tool: "exec",
          exitCode: 0,
          timedOut: false,
          outputTruncated: false,
          stdout: "rendered\n",
          stderr: "",
          backend: "local",
        }}
      />,
    );

    expect(screen.queryByText("Local")).toBeNull();
    expect(screen.queryByText("Done")).toBeNull();

    await user.click(screen.getByRole("button", { name: /python3 render.py/ }));

    expect(screen.getByText("Local")).toBeTruthy();
    expect(screen.getByText("Done")).toBeTruthy();
    expect(screen.getByLabelText("Output")).toHaveTextContent("rendered");
  });

  it("keeps a failed command folded until the reader opens it", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <ToolCommandCard
        name="exec"
        status="failed"
        preview={preview}
        result={{
          tool: "exec",
          exitCode: 101,
          timedOut: false,
          outputTruncated: false,
          stdout: "",
          stderr: "module not found\n",
          backend: "local",
        }}
      />,
    );

    const row = screen.getByRole("button", { name: /python3 render.py/ });
    expect(row.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("Exit 101")).toBeNull();
    expect(screen.queryByText(/module not found/)).toBeNull();

    await user.click(row);

    expect(row.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("Exit 101")).toBeTruthy();
    expect(screen.getByLabelText("Output")).toHaveTextContent(
      "module not found",
    );

    rerender(
      <ToolCommandCard
        name="exec"
        status="failed"
        preview={preview}
        result={{
          tool: "exec",
          exitCode: null,
          timedOut: true,
          outputTruncated: false,
          stdout: "",
          stderr: "",
          backend: "local",
        }}
      />,
    );
    expect(screen.getByText("Timed out")).toBeTruthy();
  });
});
