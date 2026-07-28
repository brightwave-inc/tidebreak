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
  CodeExecutionCredentialReadiness,
} from "../api";
import { CodeExecutionPanel } from "./CodeExecutionPanel";

function clientFor(
  config: CodeExecutionConfigInfo,
  credentials: CodeExecutionCredentialReadiness[] = [
    { provider: "e2b", has_credential: false },
    { provider: "daytona", has_credential: false },
  ],
) {
  const putCodeExecutionConfig = vi.fn().mockResolvedValue(config);
  const putCodeExecutionCredential = vi
    .fn()
    .mockImplementation((provider: string) =>
      Promise.resolve({ provider, has_credential: true }),
    );
  const deleteCodeExecutionCredential = vi
    .fn()
    .mockImplementation((provider: string) =>
      Promise.resolve({ provider, has_credential: false }),
    );
  return {
    client: {
      getCodeExecutionConfig: vi.fn().mockResolvedValue(config),
      listCodeExecutionCredentials: vi.fn().mockResolvedValue({ credentials }),
      putCodeExecutionConfig,
      putCodeExecutionCredential,
      deleteCodeExecutionCredential,
    } as unknown as ApiClient,
    putCodeExecutionConfig,
    putCodeExecutionCredential,
    deleteCodeExecutionCredential,
  };
}

afterEach(cleanup);

describe("CodeExecutionPanel", () => {
  it("saves a key per managed provider before the active selection", async () => {
    const { client, putCodeExecutionConfig, putCodeExecutionCredential } =
      clientFor({
        provider: "e2b",
        timeout_ms: 20_000,
        available: false,
        has_credential: false,
      });

    render(<CodeExecutionPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/E2B API key/), {
      target: { value: "  e2b-secret  " },
    });
    fireEvent.change(screen.getByLabelText(/Daytona API key/), {
      target: { value: "daytona-secret" },
    });
    fireEvent.change(screen.getByLabelText(/Execution timeout/), {
      target: { value: "30000" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await waitFor(() =>
      expect(putCodeExecutionCredential).toHaveBeenCalledWith(
        "e2b",
        "e2b-secret",
      ),
    );
    expect(putCodeExecutionCredential).toHaveBeenCalledWith(
      "daytona",
      "daytona-secret",
    );
    expect(putCodeExecutionConfig).toHaveBeenCalledWith({
      provider: "e2b",
      timeout_ms: 30_000,
    });
    // A provider must not go active in a pass that failed to store its key.
    expect(
      putCodeExecutionCredential.mock.invocationCallOrder[0],
    ).toBeLessThan(putCodeExecutionConfig.mock.invocationCallOrder[0]);
  });

  it("removes one provider's saved key without touching the other", async () => {
    const { client, deleteCodeExecutionCredential } = clientFor(
      {
        provider: "e2b",
        timeout_ms: 20_000,
        available: true,
        has_credential: true,
      },
      [
        { provider: "e2b", has_credential: true },
        { provider: "daytona", has_credential: true },
      ],
    );

    render(<CodeExecutionPanel client={client} />);

    fireEvent.click(
      await screen.findByRole("button", { name: "Remove saved Daytona key" }),
    );

    await waitFor(() =>
      expect(deleteCodeExecutionCredential).toHaveBeenCalledWith("daytona"),
    );
    expect(deleteCodeExecutionCredential).toHaveBeenCalledTimes(1);
  });

  it("rejects a timeout outside the bounds before touching the server", async () => {
    const { client, putCodeExecutionConfig } = clientFor({
      provider: "local",
      timeout_ms: 20_000,
      available: true,
      has_credential: false,
    });

    render(<CodeExecutionPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/Execution timeout/), {
      target: { value: "500000" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await screen.findByRole("alert");
    expect(putCodeExecutionConfig).not.toHaveBeenCalled();
  });
});
