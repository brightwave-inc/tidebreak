// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ProviderInfo } from "../api";
import { ProvidersPanel } from "./ProvidersPanel";

const compatible: ProviderInfo = {
  kind: "openai_compatible",
  enabled: true,
  has_credential: true,
  base_url: "http://127.0.0.1:1234/v1",
  models: [],
};

afterEach(cleanup);

describe("ProvidersPanel", () => {
  it("registers custom compatible models with explicit runtime limits", async () => {
    const putProvider = vi.fn().mockResolvedValue(compatible);
    const client = { putProvider } as unknown as ApiClient;

    render(
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

    render(
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

    render(
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
            reasoning_efforts: ["low", "medium", "high", "xhigh"],
          },
        ],
      }),
    );
    expect(screen.queryByPlaceholderText(/base URL/i)).not.toBeInTheDocument();
    expect(screen.getByText("Requests go directly to api.x.ai/v1.")).toBeInTheDocument();
  });

  it("does not expose custom model registration for curated providers", () => {
    render(
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

    render(
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

    render(
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
    render(
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
  });

  it("configures Gemini service-account auth without projecting the secret", async () => {
    const gemini: ProviderInfo = {
      kind: "gemini",
      enabled: false,
      has_credential: false,
      models: [],
    };
    const putProvider = vi.fn().mockResolvedValue(gemini);
    const client = { putProvider } as unknown as ApiClient;

    render(
      <ProvidersPanel
        providers={[gemini]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    const user = userEvent.setup();
    await user.click(screen.getByLabelText("Gemini credential type"));
    await user.click(
      await screen.findByRole("option", {
        name: "Google Cloud service account",
      }),
    );
    fireEvent.change(screen.getByLabelText("Vertex AI location"), {
      target: { value: "us-central1" },
    });
    fireEvent.change(screen.getByLabelText("Google service account JSON"), {
      target: { value: '  {"type":"service_account","private_key":"secret"}  ' },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Save configuration" }),
    );

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("gemini", {
        enabled: true,
        vertex_location: "us-central1",
        credential: {
          type: "service_account",
          json: '{"type":"service_account","private_key":"secret"}',
        },
      }),
    );
  });

  it("offers no credential editing on a managed profile", () => {
    const putProvider = vi.fn();
    const client = { putProvider } as unknown as ApiClient;

    render(
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
