// @vitest-environment jsdom

import type { ReactElement } from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ModelInfo, ProviderInfo } from "../api";
import { ProvidersPanel } from "./ProvidersPanel";

const compatible: ProviderInfo = {
  kind: "openai_compatible",
  enabled: true,
  has_credential: true,
  base_url: "http://127.0.0.1:1234/v1",
  models: [],
};

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

    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    fireEvent.change(screen.getByLabelText("Custom model 1 ID"), {
      target: { value: " vendor/model " },
    });
    fireEvent.change(screen.getByLabelText("Custom model 1 display name"), {
      target: { value: " Vendor Model " },
    });
    fireEvent.change(screen.getByLabelText("Custom model 1 context tokens"), {
      target: { value: "65536" },
    });
    fireEvent.change(screen.getByLabelText("Custom model 1 max output"), {
      target: { value: "8192" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Save configuration" }),
    );

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("openai_compatible", {
        enabled: true,
        base_url: "http://127.0.0.1:1234/v1",
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

  it("omits an unset display name instead of sending null", async () => {
    const putProvider = vi.fn().mockResolvedValue(compatible);
    const client = { putProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel
        providers={[compatible]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    fireEvent.change(screen.getByLabelText("Custom model 1 ID"), {
      target: { value: "vendor/model" },
    });
    // Display name left blank, which is the case the server represents by
    // omitting the key. It used to be sent as an explicit null.
    fireEvent.click(
      screen.getByRole("button", { name: "Save configuration" }),
    );

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

  it("configures xAI model capabilities without an endpoint override", async () => {
    const xai: ProviderInfo = {
      kind: "xai",
      enabled: false,
      has_credential: false,
      models: [],
    };
    const putProvider = vi.fn().mockResolvedValue(xai);
    const client = { putProvider } as unknown as ApiClient;

    renderPanel(
      <ProvidersPanel providers={[xai]} client={client} onChanged={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    fireEvent.change(screen.getByLabelText("Custom model 1 ID"), {
      target: { value: "grok-account-model" },
    });
    fireEvent.change(screen.getByPlaceholderText("API key"), {
      target: { value: "xai-key" },
    });
    fireEvent.click(screen.getByRole("checkbox", { name: "Image input" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Reasoning model" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "none" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "low" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "medium" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "high" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "xhigh" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Save configuration" }),
    );

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("xai", {
        enabled: true,
        credential: { type: "api_key", key: "xai-key" },
        models: [
          {
            id: "grok-account-model",
            display_name: undefined,
            context_window: 32_768,
            max_output_tokens: 4_096,
            input_modalities: ["text", "image"],
            supports_reasoning: true,
            reasoning_efforts: ["none", "low", "medium", "high", "xhigh"],
          },
        ],
      }),
    );
    expect(screen.queryByPlaceholderText(/base URL/i)).not.toBeInTheDocument();
    expect(screen.getByText("Requests go directly to api.x.ai/v1.")).toBeInTheDocument();
  });

  it("does not expose custom model registration for curated providers", () => {
    renderPanel(
      <ProvidersPanel
        providers={[
          {
            // `base_url` is omitted, not null, exactly as the server sends it.
            kind: "anthropic",
            enabled: false,
            has_credential: false,
            models: [],
          },
        ]}
        client={{} as ApiClient}
        onChanged={vi.fn()}
      />,
    );

    expect(
      screen.queryByRole("button", { name: "Add model" }),
    ).not.toBeInTheDocument();
  });

  it("shows fixed endpoints for direct compatible presets without editing them", () => {
    const fireworks: ProviderInfo = {
      kind: "fireworks",
      enabled: false,
      has_credential: false,
      base_url: "https://api.fireworks.ai/inference/v1",
      models: [],
    };
    const together: ProviderInfo = {
      kind: "together",
      enabled: false,
      has_credential: false,
      base_url: "https://api.together.ai/v1",
      models: [],
    };

    renderPanel(
      <ProvidersPanel
        providers={[fireworks, together]}
        client={{} as ApiClient}
        onChanged={vi.fn()}
      />,
    );

    expect(screen.getByText("Fireworks AI")).toBeInTheDocument();
    expect(screen.getByText("Together AI")).toBeInTheDocument();
    expect(
      screen.getByText(/https:\/\/api\.fireworks\.ai\/inference\/v1/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/https:\/\/api\.together\.ai\/v1/),
    ).toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/base URL/)).not.toBeInTheDocument();
  });

  it("starts ChatGPT OAuth from the OpenAI provider row", async () => {
    const openai: ProviderInfo = {
      kind: "openai",
      enabled: false,
      has_credential: false,
      models: [],
    };
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

  it("resumes a ChatGPT sign-in that started before the panel mounted", async () => {
    const getOpenaiChatgptStatus = vi
      .fn()
      .mockResolvedValueOnce({
        signed_in: false,
        pending_authorization_url: "https://auth.openai.com/oauth/authorize",
      })
      .mockResolvedValue({ signed_in: true });
    const onChanged = vi.fn();

    renderPanel(
      <ProvidersPanel
        providers={[
          { kind: "openai", enabled: false, has_credential: false, models: [] },
        ]}
        client={{ getOpenaiChatgptStatus } as unknown as ApiClient}
        onChanged={onChanged}
      />,
    );

    // No click here: the sign-in belongs to the server, and the panel has to
    // pick its completion up on its own.
    await waitFor(() => expect(onChanged).toHaveBeenCalled(), {
      timeout: 5_000,
    });
  });

  it("shows ChatGPT sign-out when OpenAI is signed in via subscription", () => {
    renderPanel(
      <ProvidersPanel
        providers={[
          {
            kind: "openai",
            enabled: true,
            has_credential: true,
            auth_mode: "chatgpt",
            models: [],
          },
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
  });

  it("lets an API-key install switch to ChatGPT sign-in", () => {
    renderPanel(
      <ProvidersPanel
        providers={[
          {
            kind: "openai",
            enabled: true,
            has_credential: true,
            auth_mode: "api_key",
            models: [],
          },
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
    const putProvider = vi.fn().mockResolvedValue({
      kind: "openai",
      enabled: true,
      has_credential: true,
      auth_mode: "api_key",
      models: [],
    });
    const client = {
      putProvider,
      getOpenaiChatgptStatus: vi.fn().mockResolvedValue({ signed_in: true }),
    } as unknown as ApiClient;
    const user = userEvent.setup();

    renderPanel(
      <ProvidersPanel
        providers={[
          {
            kind: "openai",
            enabled: true,
            has_credential: true,
            auth_mode: "chatgpt",
            models: [],
          },
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
    expect(putProvider).not.toHaveBeenCalled();
  });
});

describe("ProvidersPanel model visibility", () => {
  const anthropic: ProviderInfo = {
    kind: "anthropic",
    enabled: true,
    has_credential: true,
    models: [],
  };
  const openai: ProviderInfo = {
    kind: "openai",
    enabled: true,
    has_credential: true,
    models: [],
  };
  const model = (
    key: string,
    provider: ModelInfo["provider"],
    display_name: string,
    recommended: boolean,
  ): ModelInfo =>
    ({
      key,
      id: key.split("::")[1],
      display_name,
      provider,
      vendor: null,
      verification: "verified",
      recommended,
      available: true,
      context_window: 200_000,
      max_output_tokens: 64_000,
      input_modalities: ["text"],
      supports_reasoning: false,
      reasoning_efforts: [],
      supports_tools: true,
      supports_structured_output: true,
      multimodal: false,
    }) as unknown as ModelInfo;

  const models: ModelInfo[] = [
    model("anthropic::opus-5", "anthropic", "Claude Opus 5", true),
    model("anthropic::opus-4-8", "anthropic", "Claude Opus 4.8", false),
    model("openai::gpt-5-mini", "openai", "GPT-5 mini", false),
  ];

  function clientWith(overrides: Record<string, "show" | "hide">) {
    const putSettings = vi
      .fn()
      .mockImplementation((body: Record<string, unknown>) =>
        Promise.resolve({
          model_visibility_overrides: body.model_visibility_overrides,
        }),
      );
    const getSettings = vi
      .fn()
      .mockResolvedValue({ model_visibility_overrides: overrides });
    return {
      putSettings,
      getSettings,
      client: {
        getSettings,
        putSettings,
        // The OpenAI card resumes any sign-in the server is still holding.
        getOpenaiChatgptStatus: vi.fn().mockResolvedValue({ signed_in: false }),
      } as unknown as ApiClient,
    };
  }

  it("writes the complete deviation map, never a redundant entry", async () => {
    // A deviation for another provider is already stored: the write replaces
    // the whole map, so losing it would silently unhide that model.
    const { client, putSettings } = clientWith({
      "openai::gpt-5-mini": "show",
      "anthropic::opus-4-8": "show",
    });

    renderPanel(
      <ProvidersPanel
        providers={[anthropic]}
        models={models}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    // Unchecking a model that is only visible because of an override returns
    // it to its catalog default, which is the absence of a key.
    const legacy = await screen.findByRole("checkbox", {
      name: "Show Claude Opus 4.8",
    });
    await waitFor(() => expect(legacy).toBeChecked());
    fireEvent.click(legacy);
    await waitFor(() =>
      expect(putSettings).toHaveBeenCalledWith({
        model_visibility_overrides: { "openai::gpt-5-mini": "show" },
      }),
    );

    fireEvent.click(
      screen.getByRole("checkbox", { name: "Show Claude Opus 5" }),
    );
    await waitFor(() =>
      expect(putSettings).toHaveBeenLastCalledWith({
        model_visibility_overrides: {
          "openai::gpt-5-mini": "show",
          "anthropic::opus-5": "hide",
        },
      }),
    );
  });

  it("resets only the card's own provider keys", async () => {
    const { client, putSettings } = clientWith({
      "anthropic::opus-5": "hide",
      "anthropic::opus-4-8": "show",
      "openai::gpt-5-mini": "show",
    });

    renderPanel(
      <ProvidersPanel
        providers={[anthropic, openai]}
        models={models}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    const [reset] = await screen.findAllByRole("button", {
      name: "Reset to defaults",
    });
    fireEvent.click(reset);

    await waitFor(() =>
      expect(putSettings).toHaveBeenCalledWith({
        model_visibility_overrides: { "openai::gpt-5-mini": "show" },
      }),
    );
  });

  it("expands the card a deep link names", async () => {
    const { client } = clientWith({});

    render(
      <ProvidersPanel
        providers={[anthropic, openai]}
        models={models}
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
