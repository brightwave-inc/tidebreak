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
  ExecConfigInfo,
} from "./api";
import { WelcomeState } from "./WelcomeState";

afterEach(cleanup);

function executionClient(
  providers: ExecConfigInfo["providers"],
): Pick<ApiClient, "getExecConfig"> {
  return {
    getExecConfig: vi.fn().mockResolvedValue({
      providers,
    } as ExecConfigInfo),
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
      "Brief this week's AI news",
      "Compare two public products",
      "Build a local planner",
      "Research in parallel",
    ]) {
      expect(screen.getByRole("button", { name: new RegExp(label) })).toBeInTheDocument();
    }

    // A starter has to stand on its own: it names the work and starts, rather
    // than asking what to attach or what the task is.
    fireEvent.click(
      screen.getByRole("button", { name: /Brief this week's AI news/ }),
    );
    expect(onSelectPrompt).toHaveBeenCalledWith(
      expect.stringMatching(/Search the web/),
      { enableInternet: true },
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
      expect(client.getExecConfig).toHaveBeenCalled(),
    );
    expect(
      screen.queryByText(/Files staged for a run leave this machine/i),
    ).toBeNull();
  });
});
