// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
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
  it("renders the greeting and starter prompts when a handler is provided", () => {
    render(<WelcomeState onSelectPrompt={vi.fn()} />);

    expect(
      screen.getByRole("heading", { name: "How can I help?" }),
    ).toBeInTheDocument();
    expect(screen.getByText("What can you help me with?")).toBeInTheDocument();
    expect(screen.getByText("Draft a plan")).toBeInTheDocument();
    expect(
      screen.getByRole("region", { name: "Start a chat" }),
    ).toBeInTheDocument();
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
