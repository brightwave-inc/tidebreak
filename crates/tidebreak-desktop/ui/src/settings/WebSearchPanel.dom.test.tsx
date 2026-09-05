// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import userEvent from "@testing-library/user-event";
import type {
  ApiClient,
  WebSearchConfigInfo,
  WebSearchCredentialReadiness,
} from "../api";
import { WebSearchPanel } from "./WebSearchPanel";

function clientFor(
  config: WebSearchConfigInfo,
  credentials: WebSearchCredentialReadiness[] = [
    { provider: "exa", has_credential: false },
    { provider: "tavily", has_credential: false },
    { provider: "firecrawl", has_credential: false },
  ],
) {
  const putWebSearchConfig = vi.fn().mockResolvedValue(config);
  const putWebSearchCredential = vi
    .fn()
    .mockImplementation((provider: string) =>
      Promise.resolve({ provider, has_credential: true }),
    );
  const deleteWebSearchCredential = vi
    .fn()
    .mockImplementation((provider: string) =>
      Promise.resolve({ provider, has_credential: false }),
    );
  return {
    client: {
      getWebSearchConfig: vi.fn().mockResolvedValue(config),
      listWebSearchCredentials: vi.fn().mockResolvedValue({ credentials }),
      putWebSearchConfig,
      putWebSearchCredential,
      deleteWebSearchCredential,
    } as unknown as ApiClient,
    putWebSearchConfig,
    putWebSearchCredential,
    deleteWebSearchCredential,
  };
}

afterEach(cleanup);

describe("WebSearchPanel", () => {
  it("saves a key per provider and the active selection in one pass, in seconds", async () => {
    const { client, putWebSearchConfig, putWebSearchCredential } = clientFor({
      provider: "exa",
      has_credential: false,
      available: false,
      timeout_ms: 20_000,
      mode: "automatic",
    });

    render(<WebSearchPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/Exa API key/), {
      target: { value: "  exa-secret  " },
    });
    fireEvent.change(screen.getByLabelText(/Tavily API key/), {
      target: { value: "tavily-secret" },
    });
    fireEvent.change(screen.getByLabelText(/Firecrawl API key/), {
      target: { value: "firecrawl-secret" },
    });
    fireEvent.change(screen.getByLabelText(/Request timeout/), {
      target: { value: "30" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await waitFor(() =>
      expect(putWebSearchCredential).toHaveBeenCalledWith("exa", "exa-secret"),
    );
    expect(putWebSearchCredential).toHaveBeenCalledWith(
      "tavily",
      "tavily-secret",
    );
    expect(putWebSearchCredential).toHaveBeenCalledWith(
      "firecrawl",
      "firecrawl-secret",
    );
    expect(putWebSearchConfig).toHaveBeenCalledWith({
      mode: "automatic",
      provider: "exa",
      timeout_ms: 30_000,
      searxng_base_url: null,
    });
    // A provider must not go active in a pass that failed to store its key.
    expect(putWebSearchCredential.mock.invocationCallOrder[0]).toBeLessThan(
      putWebSearchConfig.mock.invocationCallOrder[0],
    );
  });

  it("removes one provider's saved key without touching the other", async () => {
    const { client, deleteWebSearchCredential } = clientFor(
      {
        provider: "exa",
        has_credential: true,
        available: true,
        timeout_ms: 20_000,
        mode: "automatic",
      },
      [
        { provider: "exa", has_credential: true },
        { provider: "tavily", has_credential: true },
      ],
    );

    render(<WebSearchPanel client={client} />);

    fireEvent.click(
      await screen.findByRole("button", { name: "Remove saved Tavily key" }),
    );

    await waitFor(() =>
      expect(deleteWebSearchCredential).toHaveBeenCalledWith("tavily"),
    );
    expect(deleteWebSearchCredential).toHaveBeenCalledTimes(1);
  });

  // The self-hosted provider is configured by address, not by key: it has no
  // credential field, and losing the address field would leave it selected and
  // unusable with nothing on screen to repair it.
  it("saves the self-hosted instance URL and offers it no key field", async () => {
    const { client, putWebSearchConfig, putWebSearchCredential } = clientFor({
      provider: "searxng",
      has_credential: false,
      available: false,
      timeout_ms: 20_000,
      mode: "automatic",
    });

    render(<WebSearchPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/SearXNG instance URL/), {
      target: { value: "  http://localhost:8888  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await waitFor(() =>
      expect(putWebSearchConfig).toHaveBeenCalledWith({
        mode: "automatic",
        provider: "searxng",
        timeout_ms: 20_000,
        searxng_base_url: "http://localhost:8888",
      }),
    );
    expect(putWebSearchCredential).not.toHaveBeenCalled();
    expect(screen.queryByLabelText(/SearXNG API key/)).toBeNull();
  });

  // The mode decides who searches at all, so it has to survive a round trip:
  // a panel that loaded one mode and saved another would silently retarget
  // every chat's search.
  it("loads the stored mode and saves the one the user picked", async () => {
    const user = userEvent.setup();
    const { client, putWebSearchConfig } = clientFor({
      provider: "exa",
      has_credential: true,
      available: true,
      timeout_ms: 20_000,
      mode: "host",
    });

    render(<WebSearchPanel client={client} />);

    const modeSelect = await screen.findByRole("combobox", {
      name: "Search mode",
    });
    expect(modeSelect).toHaveTextContent("Configured provider");

    await user.click(modeSelect);
    await user.click(
      screen.getByRole("option", { name: "Model provider (built-in)" }),
    );
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    await waitFor(() =>
      expect(putWebSearchConfig).toHaveBeenCalledWith({
        mode: "vendor",
        provider: "exa",
        timeout_ms: 20_000,
        searxng_base_url: null,
      }),
    );
  });

  it("rejects a timeout outside the bounds before touching the server", async () => {
    const { client, putWebSearchConfig } = clientFor({
      provider: "exa",
      has_credential: true,
      available: true,
      timeout_ms: 20_000,
      mode: "automatic",
    });

    render(<WebSearchPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/Request timeout/), {
      target: { value: "90" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await screen.findByRole("alert");
    expect(putWebSearchConfig).not.toHaveBeenCalled();
  });

  /**
   * The verdict is the one place the panel says whether search will actually
   * run. Automatic falls back to the chat's own model provider, so an install
   * with no key is working rather than broken — reporting it as unconfigured
   * sends the reader to buy an API key they do not need.
   */
  it("reports automatic as working when only the model's own search is left", async () => {
    const { client } = clientFor({
      has_credential: false,
      available: false,
      timeout_ms: 20_000,
      mode: "automatic",
    });

    render(<WebSearchPanel client={client} />);

    expect(await screen.findByText(/Built-in search only/)).toBeTruthy();
    expect(
      screen.getByText(/searches through the model it is running on/),
    ).toBeTruthy();
  });

  /**
   * Explicit host mode is the one state that still strands a chat: the operator
   * ruled out the model provider, so an unkeyed engine really does mean no
   * search, and the panel must keep saying so.
   */
  it("still reports explicit host mode without a key as unconfigured", async () => {
    const { client } = clientFor({
      provider: "exa",
      has_credential: false,
      available: false,
      timeout_ms: 20_000,
      mode: "host",
    });

    render(<WebSearchPanel client={client} />);

    expect(await screen.findByText(/Not configured/)).toBeTruthy();
    expect(screen.getByText(/needs an API key/)).toBeTruthy();
  });
});
