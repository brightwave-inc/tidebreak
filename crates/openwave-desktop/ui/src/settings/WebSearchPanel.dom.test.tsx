// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, WebSearchConfigInfo } from "../api";
import { WebSearchPanel } from "./WebSearchPanel";

function clientFor(config: WebSearchConfigInfo) {
  const putWebSearchConfig = vi.fn().mockResolvedValue(config);
  const putWebSearchCredential = vi.fn().mockResolvedValue({
    provider: "exa",
    has_credential: true,
  });
  const credentials = {
    credentials: [
      {
        provider: config.provider ?? "exa",
        has_credential: config.has_credential,
      },
    ],
  };
  return {
    client: {
      getWebSearchConfig: vi.fn().mockResolvedValue(config),
      listWebSearchCredentials: vi.fn().mockResolvedValue(credentials),
      putWebSearchConfig,
      putWebSearchCredential,
    } as unknown as ApiClient,
    putWebSearchConfig,
    putWebSearchCredential,
  };
}

afterEach(cleanup);

describe("WebSearchPanel", () => {
  it("saves the key and the configuration in one pass, in seconds", async () => {
    const { client, putWebSearchConfig, putWebSearchCredential } = clientFor({
      provider: "exa",
      has_credential: false,
      timeout_ms: 20_000,
    });

    render(<WebSearchPanel client={client} />);

    const key = await screen.findByLabelText(/API key/);
    fireEvent.change(key, { target: { value: "  exa-secret  " } });
    fireEvent.change(screen.getByLabelText(/Request timeout/), {
      target: { value: "30" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await waitFor(() =>
      expect(putWebSearchCredential).toHaveBeenCalledWith("exa", "exa-secret"),
    );
    expect(putWebSearchConfig).toHaveBeenCalledWith({
      provider: "exa",
      timeout_ms: 30_000,
    });
  });

  it("rejects a timeout outside the bounds before touching the server", async () => {
    const { client, putWebSearchConfig } = clientFor({
      provider: "exa",
      has_credential: true,
      timeout_ms: 20_000,
    });

    render(<WebSearchPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/Request timeout/), {
      target: { value: "90" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await screen.findByRole("alert");
    expect(putWebSearchConfig).not.toHaveBeenCalled();
  });
});
