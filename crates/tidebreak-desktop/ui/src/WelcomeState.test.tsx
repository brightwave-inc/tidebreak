// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApiClient,
  CodeExecutionConfigInfo,
} from "./api";
import { WelcomeState } from "./WelcomeState";

afterEach(cleanup);

function executionClient(
  providers: CodeExecutionConfigInfo["providers"],
): Pick<ApiClient, "getCodeExecutionConfig"> {
  return {
    getCodeExecutionConfig: vi.fn().mockResolvedValue({
      providers,
    } as CodeExecutionConfigInfo),
  };
}

describe("WelcomeState", () => {
  it("offers the launch outcomes as starter prompts when a handler is provided", () => {
    const onSelectPrompt = vi.fn();
    render(<WelcomeState onSelectPrompt={onSelectPrompt} />);

    expect(
      screen.getByRole("heading", { name: "How can I help?" }),
    ).toBeInTheDocument();
    for (const label of [
      "Write a report from files",
      "Analyze a spreadsheet",
      "Delegate work in parallel",
      "Turn a folder into an app",
    ]) {
      expect(screen.getByRole("button", { name: label })).toBeInTheDocument();
    }
    // Nothing on home offers to search sources any more.
    expect(screen.queryByText(/search/i)).toBeNull();

    // A prompt has to stand on its own before anything is attached: it asks
    // for the input it is missing rather than assuming hidden context.
    fireEvent.click(
      screen.getByRole("button", { name: "Write a report from files" }),
    );
    expect(onSelectPrompt).toHaveBeenCalledWith(
      expect.stringMatching(/Tell me what to attach/),
    );
  });

  it("offers the first-task walkthrough when home provides it", () => {
    const onStartWalkthrough = vi.fn();
    render(
      <WelcomeState
        heading="Welcome to Tidebreak"
        description="Set up the agent before the first task."
        onStartWalkthrough={onStartWalkthrough}
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Welcome to Tidebreak" }),
    ).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Set up your first task" }),
    );
    expect(onStartWalkthrough).toHaveBeenCalledOnce();
  });

  it("omits the starter prompts when no handler is provided", () => {
    const { container } = render(<WelcomeState />);

    expect(
      screen.getByRole("heading", { name: "How can I help?" }),
    ).toBeInTheDocument();
    expect(container.querySelector(".welcome-prompts")).toBeNull();
  });

  it("discloses cloud execution and staged-file uploads when Local is unsupported", async () => {
    render(
      <WelcomeState
        executionConfigClient={executionClient([
          {
            provider: "local",
            available: false,
            unavailable_reason: "unsupported_platform",
          },
          {
            provider: "e2b",
            available: false,
            unavailable_reason: "missing_credential",
          },
          {
            provider: "daytona",
            available: false,
            unavailable_reason: "missing_credential",
          },
        ])}
      />,
    );

    expect(
      await screen.findByText(/Files staged for a run leave this machine/i),
    ).toBeInTheDocument();
  });

  it("keeps the macOS welcome copy unchanged when Local is available", async () => {
    const client = executionClient([
      { provider: "local", available: true },
      { provider: "e2b", available: true },
      { provider: "daytona", available: true },
    ]);
    render(<WelcomeState executionConfigClient={client} />);

    await waitFor(() =>
      expect(client.getCodeExecutionConfig).toHaveBeenCalled(),
    );
    expect(
      screen.queryByText(/Files staged for a run leave this machine/i),
    ).toBeNull();
  });
});
