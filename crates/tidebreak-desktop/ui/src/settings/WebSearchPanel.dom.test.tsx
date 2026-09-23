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
  it("saves each key beside its field and the timeout on blur", async () => {
    const exaKey = ["exa", "key"].join("-");
    const tavilyKey = ["tavily", "key"].join("-");
    const firecrawlKey = ["firecrawl", "key"].join("-");
    const { client, putWebSearchConfig, putWebSearchCredential } = clientFor({
      provider: "exa",
      has_credential: false,
      available: false,
      timeout_ms: 20_000,
      mode: "automatic",
    });

    render(<WebSearchPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/Exa API key/), {
      target: { value: `  ${exaKey}  ` },
    });
    fireEvent.click(screen.getAllByRole("button", { name: "Save key" })[0]);
    await waitFor(() =>
      expect(putWebSearchCredential).toHaveBeenCalledWith("exa", exaKey),
    );

    fireEvent.change(screen.getByLabelText(/Tavily API key/), {
      target: { value: tavilyKey },
    });
    fireEvent.click(screen.getAllByRole("button", { name: "Save key" })[1]);
    await waitFor(() =>
      expect(putWebSearchCredential).toHaveBeenCalledWith("tavily", tavilyKey),
    );

    fireEvent.change(screen.getByLabelText(/Firecrawl API key/), {
      target: { value: firecrawlKey },
    });
    fireEvent.click(screen.getAllByRole("button", { name: "Save key" })[2]);
    await waitFor(() =>
      expect(putWebSearchCredential).toHaveBeenCalledWith(
        "firecrawl",
        firecrawlKey,
      ),
    );

    fireEvent.change(screen.getByLabelText(/Request timeout/), {
      target: { value: "30" },
    });
    fireEvent.blur(screen.getByLabelText(/Request timeout/));

    await waitFor(() =>
      expect(putWebSearchConfig).toHaveBeenCalledWith({
        timeout_ms: 30_000,
      }),
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
    fireEvent.blur(screen.getByLabelText(/SearXNG instance URL/));

    await waitFor(() =>
      expect(putWebSearchConfig).toHaveBeenCalledWith({
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

    await waitFor(() =>
      expect(putWebSearchConfig).toHaveBeenCalledWith({
        mode: "vendor",
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
    fireEvent.blur(screen.getByLabelText(/Request timeout/));

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

  it("refuses to activate a provider that still has no saved key", async () => {
    const user = userEvent.setup();
    const { client, putWebSearchConfig } = clientFor(
      {
        provider: "exa",
        has_credential: true,
        available: true,
        timeout_ms: 20_000,
        mode: "host",
      },
      [
        { provider: "exa", has_credential: true },
        { provider: "tavily", has_credential: false },
      ],
    );

    render(<WebSearchPanel client={client} />);

    const providerSelect = await screen.findByRole("combobox", {
      name: "Provider",
    });
    await user.click(providerSelect);
    await user.click(screen.getByRole("option", { name: "Tavily" }));

    expect(
      await screen.findByText(
        /Tavily needs an API key before you can make it active/,
      ),
    ).toBeTruthy();
    expect(putWebSearchConfig).not.toHaveBeenCalled();
  });

  it("keeps SearXNG selected and search on when the instance URL is cleared", async () => {
    const { client, putWebSearchConfig } = clientFor({
      provider: "searxng",
      has_credential: false,
      available: true,
      timeout_ms: 20_000,
      mode: "host",
      searxng_base_url: "http://localhost:8888",
    });

    render(<WebSearchPanel client={client} />);

    const urlField = await screen.findByLabelText(/SearXNG instance URL/);
    fireEvent.change(urlField, { target: { value: "" } });
    fireEvent.blur(urlField);

    expect(
      await screen.findByText(/SearXNG needs an instance URL/),
    ).toBeTruthy();
    expect(putWebSearchConfig).not.toHaveBeenCalled();
    expect(
      screen.getByRole("combobox", { name: "Provider" }),
    ).toHaveTextContent("SearXNG");
    expect(
      screen.getByRole("combobox", { name: "Search mode" }),
    ).toHaveTextContent("Configured provider");
    expect(urlField).toHaveValue("http://localhost:8888");
  });
});
