// @vitest-environment jsdom

import type { ReactElement } from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApiClient,
  ModelInfo,
  ProviderInfo,
  ProviderTestResult,
} from "../api";
import { providerInfoFixture } from "../stories/fixtures";
import { ProvidersPanel } from "./ProvidersPanel";

const compatible: ProviderInfo = providerInfoFixture("openai_compatible", {
  enabled: true,
  has_credential: true,
  base_url: "http://127.0.0.1:1234/v1",
});

const connectedTest: ProviderTestResult = {
  outcome: "connected",
  message: "Anthropic accepted the saved key.",
  tested_at: new Date().toISOString(),
};

const rejectedTest: ProviderTestResult = {
  outcome: "key_rejected",
  message:
    "Anthropic rejected the saved API key (HTTP 401). Save a valid key, then test again.",
  status: 401,
  tested_at: new Date().toISOString(),
};

function grok(id: string, name: string): ModelInfo {
  return {
    key: `xai::${id}`,
    id,
    display_name: name,
    provider: "xai",
    vendor: null,
    verification: "unverified",
    available: false,
    context_window: 500_000,
    max_output_tokens: 32_768,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: false,
    reasoning_efforts: ["low", "medium", "high", "xhigh"],
    multimodal: true,
    recommended: true,
  };
}

/**
 * Cards open collapsed, so every assertion about a card's contents starts by
 * opening it — which is what a reader does too.
 */
function renderPanel(ui: ReactElement) {
  const result = render(ui);
  for (const header of screen.queryAllByRole("button", { name: /^Expand / })) {
    fireEvent.click(header);
  }
  return result;
}

/** The Add model dialog, filled in and submitted. */
async function addModel(fields: {
  id: string;
  displayName?: string;
  contextWindow?: string;
  maxOutput?: string;
}) {
  fireEvent.click(screen.getByRole("button", { name: "Add model" }));
  const dialog = await screen.findByRole("dialog");
  fireEvent.change(within(dialog).getByLabelText("Model ID"), {
    target: { value: fields.id },
  });
  if (fields.displayName !== undefined) {
    fireEvent.change(within(dialog).getByLabelText("Display name"), {
      target: { value: fields.displayName },
    });
  }
  if (fields.contextWindow !== undefined) {
    fireEvent.change(within(dialog).getByLabelText("Context window"), {
      target: { value: fields.contextWindow },
    });
  }
  if (fields.maxOutput !== undefined) {
    fireEvent.change(within(dialog).getByLabelText("Max output"), {
      target: { value: fields.maxOutput },
    });
  }
  fireEvent.click(within(dialog).getByRole("button", { name: "Add model" }));
}

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

describe("ProvidersPanel", () => {
  it("registers custom compatible models with explicit runtime limits", async () => {
    const putProvider = vi.fn().mockResolvedValue(compatible);
    const client = { putProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[compatible]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    await addModel({
      id: " vendor/model ",
      displayName: " Vendor Model ",
      contextWindow: "65536",
      maxOutput: "8,192",
    });

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("openai_compatible", {
        models: [
          {
            id: "vendor/model",
            display_name: "Vendor Model",
            context_window: 65_536,
            max_output_tokens: 8_192,
            input_modalities: ["text"],
            supports_reasoning: false,
            reasoning_efforts: [],
          },
        ],
      }),
    );
  });

  it("omits an unset display name and fills blank limits with the defaults", async () => {
    const putProvider = vi.fn().mockResolvedValue(compatible);
    const client = { putProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[compatible]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    // Display name and both limits left blank. The display name is the case
    // the server represents by omitting the key.
    await addModel({ id: "vendor/model" });

    await waitFor(() => expect(putProvider).toHaveBeenCalled());

    // Asserted through JSON rather than the argument object: `toEqual` treats an
    // absent key and an explicit `undefined` as the same thing, so only the
    // serialized form shows what actually reaches the server.
    const [, body] = putProvider.mock.calls[0];
    const sent = JSON.parse(JSON.stringify(body));
    expect(sent.models[0]).toEqual({
      id: "vendor/model",
      context_window: 32_768,
      max_output_tokens: 4_096,
      input_modalities: ["text"],
      supports_reasoning: false,
      reasoning_efforts: [],
    });
    expect("display_name" in sent.models[0]).toBe(false);
  });

  it("lists built-in Grok models and saves an xAI key without touching custom models", async () => {
    const xai = providerInfoFixture("xai");
    const putProvider = vi.fn().mockResolvedValue({
      ...xai,
      enabled: true,
      has_credential: true,
    });
    const client = { putProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[xai]}
        models={[grok("grok-4.7", "Grok 4.7"), grok("grok-4.5", "Grok 4.5")]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    expect(
      screen.getByText("Built in: Grok 4.7, Grok 4.5"),
    ).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("API key"), {
      target: { value: "xai-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save configuration" }));

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("xai", {
        enabled: true,
        credential: { type: "api_key", key: "xai-key" },
      }),
    );
    expect(screen.queryByPlaceholderText(/base URL/i)).not.toBeInTheDocument();
    expect(
      screen.getByText("Requests go directly to api.x.ai/v1."),
    ).toBeInTheDocument();
  });

  it("offers custom models on every direct provider", () => {
    renderPanel(
      <ProvidersPanel
        providers={[
          // `base_url` is omitted, not null, exactly as the server sends it.
          providerInfoFixture("anthropic"),
          providerInfoFixture("gemini", { has_credential: true }),
          providerInfoFixture("model_gateway", {
            enabled: true,
            has_credential: true,
          }),
        ]}
        client={{} as ApiClient}
        onChanged={vi.fn()}
      />,
    );

    // The gateway has its own settings page, so only the two direct
    // providers render a card here, and each offers both actions.
    expect(screen.getAllByRole("button", { name: "Add model" })).toHaveLength(
      2,
    );
    const find = screen.getAllByRole("button", { name: "Find models" });
    expect(find).toHaveLength(2);
    // Finding models spends the saved key, so a provider without one waits.
    expect(find[0]).toBeDisabled();
    expect(find[1]).toBeEnabled();
    expect(
      screen.getByText(
        /Save an API key to find the models this provider serves\./,
      ),
    ).toBeInTheDocument();
  });

  it("saves an Ollama daemon without a credential", async () => {
    const ollama = providerInfoFixture("ollama", {
      base_url: "http://127.0.0.1:11434/v1",
    });
    const putProvider = vi.fn().mockResolvedValue({
      ...ollama,
      enabled: true,
    });
    const testProvider = vi.fn().mockResolvedValue(connectedTest);
    const client = { putProvider, testProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[ollama]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    expect(screen.getByText("Ollama")).toBeInTheDocument();
    expect(
      screen.getByText("No API key required for a local Ollama"),
    ).toBeInTheDocument();
    expect(
      screen.getByPlaceholderText("API key (optional)"),
    ).toBeInTheDocument();
    // A local daemon needs no key to list its models.
    expect(screen.getByRole("button", { name: "Find models" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Save configuration" }));

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("ollama", {
        enabled: true,
        base_url: "http://127.0.0.1:11434/v1",
        allow_loopback_http: false,
      }),
    );
    // The save is when a stopped daemon should show up.
    await waitFor(() => expect(testProvider).toHaveBeenCalledWith("ollama"));
  });

  it("saves a local OpenAI-compatible server with no key", async () => {
    const empty = providerInfoFixture("openai_compatible");
    const saved = providerInfoFixture("openai_compatible", {
      enabled: true,
      base_url: "http://127.0.0.1:1234/v1",
    });
    const putProvider = vi.fn().mockResolvedValue(saved);
    const testProvider = vi.fn().mockResolvedValue(connectedTest);
    const client = { putProvider, testProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[empty]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    expect(
      screen.getByText("No API key required for a server on this computer"),
    ).toBeInTheDocument();
    expect(screen.getByPlaceholderText("API key (optional)")).toHaveValue("");
    fireEvent.change(
      screen.getByPlaceholderText("Base URL, such as http://127.0.0.1:1234/v1"),
      { target: { value: "http://127.0.0.1:1234/v1" } },
    );
    // No key, so no clear-text consent to ask for.
    expect(
      screen.queryByText("Send the key over HTTP to this computer?"),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save configuration" }));

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("openai_compatible", {
        enabled: true,
        base_url: "http://127.0.0.1:1234/v1",
        allow_loopback_http: false,
      }),
    );
    await waitFor(() =>
      expect(testProvider).toHaveBeenCalledWith("openai_compatible"),
    );
  });

  it("asks before sending a key over HTTP to this computer", async () => {
    const compatibleLocal = providerInfoFixture("openai_compatible", {
      enabled: true,
      base_url: "http://127.0.0.1:1234/v1",
    });
    const putProvider = vi
      .fn()
      .mockResolvedValue({ ...compatibleLocal, has_credential: true });
    const testProvider = vi.fn().mockResolvedValue(connectedTest);
    const client = { putProvider, testProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[compatibleLocal]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "local-server-key" },
    });
    expect(
      screen.getByText("Send the key over HTTP to this computer?"),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save configuration" }));
    expect(
      await screen.findByText(
        "Confirm that Tidebreak may send the key over HTTP to this computer, or use HTTPS.",
      ),
    ).toBeInTheDocument();
    expect(putProvider).not.toHaveBeenCalled();

    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "Send the key in clear text to this loopback address",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Save configuration" }));

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("openai_compatible", {
        enabled: true,
        base_url: "http://127.0.0.1:1234/v1",
        credential: { type: "api_key", key: "local-server-key" },
        allow_loopback_http: true,
      }),
    );
  });

  it("tests a saved key and says what the provider answered", async () => {
    const anthropic = providerInfoFixture("anthropic", {
      enabled: true,
      has_credential: true,
    });
    const testProvider = vi.fn().mockResolvedValue(rejectedTest);
    const onChanged = vi.fn();
    const client = { testProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[anthropic]}
        client={client}
        onChanged={onChanged}
      />,
    );

    // Configured but never tested: the badge does not claim a connection.
    expect(screen.getByText("Not tested")).toBeInTheDocument();
    expect(screen.queryByText("Connected")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Test" }));

    expect(await screen.findAllByText("Key rejected")).not.toHaveLength(0);
    expect(
      screen.getByText(
        /Anthropic rejected the saved API key \(HTTP 401\)\. Save a valid key, then test again\. Tested just now\./,
      ),
    ).toBeInTheDocument();
    expect(testProvider).toHaveBeenCalledWith("anthropic");
    // The provider list re-reads so the recorded test comes back with it.
    expect(onChanged).toHaveBeenCalled();
  });

  it("drives the badge from the last recorded test", () => {
    renderPanel(
      <ProvidersPanel
        providers={[
          providerInfoFixture("anthropic", {
            enabled: true,
            has_credential: true,
            last_test: connectedTest,
          }),
          providerInfoFixture("gemini", {
            enabled: true,
            has_credential: true,
            last_test: { ...rejectedTest, outcome: "rate_limited" },
          }),
          providerInfoFixture("xai", {
            enabled: false,
            has_credential: true,
            last_test: connectedTest,
          }),
        ]}
        client={{} as ApiClient}
        onChanged={vi.fn()}
      />,
    );

    const header = (name: string) =>
      screen.getByRole("button", { name: `Collapse ${name}` });
    expect(within(header("Anthropic")).getByText("Connected")).toBeVisible();
    expect(
      within(header("Google Gemini")).getByText("Rate limited"),
    ).toBeVisible();
    // Switched off, so the last test no longer describes anything that runs.
    expect(within(header("xAI")).getByText("Not connected")).toBeVisible();
  });

  it("shows fixed endpoints for direct compatible presets without editing them", () => {
    renderPanel(
      <ProvidersPanel
        providers={[
          providerInfoFixture("fireworks", {
            base_url: "https://api.fireworks.ai/inference/v1",
          }),
          providerInfoFixture("together", {
            base_url: "https://api.together.ai/v1",
          }),
          providerInfoFixture("openrouter", {
            base_url: "https://openrouter.ai/api/v1",
          }),
        ]}
        client={{} as ApiClient}
        onChanged={vi.fn()}
      />,
    );

    expect(screen.getByText("Fireworks AI")).toBeInTheDocument();
    expect(screen.getByText("Together AI")).toBeInTheDocument();
    expect(screen.getByText("OpenRouter")).toBeInTheDocument();
    expect(
      screen.getByText(/https:\/\/api\.fireworks\.ai\/inference\/v1/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/https:\/\/api\.together\.ai\/v1/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/https:\/\/openrouter\.ai\/api\/v1/),
    ).toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/base URL/)).not.toBeInTheDocument();
  });

  it("saves an OpenRouter key without an endpoint override", async () => {
    const openrouter = providerInfoFixture("openrouter", {
      base_url: "https://openrouter.ai/api/v1",
    });
    const putProvider = vi.fn().mockResolvedValue({
      ...openrouter,
      enabled: true,
      has_credential: true,
    });
    const client = { putProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[openrouter]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    expect(
      screen.getByText(/https:\/\/openrouter\.ai\/api\/v1/),
    ).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("API key"), {
      target: { value: "sk-or" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save configuration" }));

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("openrouter", {
        enabled: true,
        credential: { type: "api_key", key: "sk-or" },
      }),
    );
    expect(screen.queryByPlaceholderText(/base URL/i)).not.toBeInTheDocument();
  });

  it("starts ChatGPT OAuth from the OpenAI provider row", async () => {
    const openai = providerInfoFixture("openai");
    const openaiChatgptSignIn = vi.fn().mockResolvedValue({
      authorization_url: "https://auth.openai.com/oauth/authorize?x=1",
    });
    const getOpenaiChatgptStatus = vi
      .fn()
      .mockResolvedValue({ signed_in: false });
    const client = {
      openaiChatgptSignIn,
      getOpenaiChatgptStatus,
      putProvider: vi.fn(),
    } as unknown as ApiClient;
    const open = vi.fn();
    vi.stubGlobal("open", open);
    const user = userEvent.setup();

    renderPanel(
      <ProvidersPanel
        providers={[openai]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    expect(screen.getByText("No credential")).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Sign in with ChatGPT" }),
    );
    await waitFor(() => expect(openaiChatgptSignIn).toHaveBeenCalled());
    expect(open).toHaveBeenCalledWith(
      "https://auth.openai.com/oauth/authorize?x=1",
      "_blank",
      "noreferrer,noopener",
    );
    vi.unstubAllGlobals();
  });

  it("shows ChatGPT sign-out when OpenAI is signed in via subscription", () => {
    renderPanel(
      <ProvidersPanel
        providers={[
          providerInfoFixture("openai", {
            enabled: true,
            has_credential: true,
            auth_mode: "chatgpt",
          }),
        ]}
        client={{} as ApiClient}
        onChanged={vi.fn()}
      />,
    );

    expect(screen.getByText("Signed in with ChatGPT")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Sign out of ChatGPT" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Sign in with ChatGPT" }),
    ).not.toBeInTheDocument();
    // API key path stays available so the reader can switch modes without
    // signing out first.
    expect(
      screen.getByRole("button", { name: "Switch to API key" }),
    ).toBeInTheDocument();
    expect(
      screen.getByPlaceholderText("Paste an API key to switch from ChatGPT"),
    ).toBeInTheDocument();
    // The subscription cannot list models, and the card says why.
    expect(screen.getByRole("button", { name: "Find models" })).toBeDisabled();
    expect(
      screen.getByText(
        /Finding models needs an OpenAI API key\. ChatGPT sign-in cannot list models\./,
      ),
    ).toBeInTheDocument();
  });

  it("lets an API-key install switch to ChatGPT sign-in", () => {
    renderPanel(
      <ProvidersPanel
        providers={[
          providerInfoFixture("openai", {
            enabled: true,
            has_credential: true,
            auth_mode: "api_key",
          }),
        ]}
        client={
          {
            getOpenaiChatgptStatus: vi
              .fn()
              .mockResolvedValue({ signed_in: false }),
          } as unknown as ApiClient
        }
        onChanged={vi.fn()}
      />,
    );

    expect(screen.getByText("API key set")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Switch to ChatGPT sign-in" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save API key" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Clear" })).toBeInTheDocument();
  });

  it("saves an API key while signed in with ChatGPT to switch modes", async () => {
    const putProvider = vi.fn().mockResolvedValue(
      providerInfoFixture("openai", {
        enabled: true,
        has_credential: true,
        auth_mode: "api_key",
      }),
    );
    const client = {
      putProvider,
      getOpenaiChatgptStatus: vi.fn().mockResolvedValue({ signed_in: true }),
    } as unknown as ApiClient;
    const user = userEvent.setup();

    renderPanel(
      <ProvidersPanel
        providers={[
          providerInfoFixture("openai", {
            enabled: true,
            has_credential: true,
            auth_mode: "chatgpt",
          }),
        ]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    await user.type(
      screen.getByPlaceholderText("Paste an API key to switch from ChatGPT"),
      "sk-switch",
    );
    await user.click(screen.getByRole("button", { name: "Switch to API key" }));

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("openai", {
        enabled: true,
        credential: { type: "api_key", key: "sk-switch" },
      }),
    );
  });

  it("offers no credential editing on a managed profile", () => {
    const putProvider = vi.fn();
    const client = { putProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[compatible]}
        client={client}
        managed
        onChanged={vi.fn()}
      />,
    );

    expect(
      screen.getByText(/configured by your organization/i),
    ).toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Save configuration" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Add model" }),
    ).not.toBeInTheDocument();
    expect(putProvider).not.toHaveBeenCalled();
  });
});

describe("ProvidersPanel", () => {
  const anthropic = providerInfoFixture("anthropic", {
    enabled: true,
    has_credential: true,
  });
  const openai = providerInfoFixture("openai", {
    enabled: true,
    has_credential: true,
  });

  it("expands the card a deep link names", async () => {
    const client = {
      getOpenaiChatgptStatus: vi.fn().mockResolvedValue({ signed_in: false }),
    } as unknown as ApiClient;

    render(
      <ProvidersPanel
        providers={[anthropic, openai]}
        models={[]}
        client={client}
        onChanged={vi.fn()}
        expandProvider="openai"
      />,
    );

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Collapse OpenAI" }),
      ).toHaveAttribute("aria-expanded", "true"),
    );
    expect(
      screen.getByRole("button", { name: "Expand Anthropic" }),
    ).toHaveAttribute("aria-expanded", "false");
  });
});
