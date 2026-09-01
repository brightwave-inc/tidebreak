// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApiClient,
  ModelInfo,
  ModelRoleInfo,
  RuntimeSettings,
} from "../api";
import { ModelsPanel } from "./ModelsPanel";

const models: ModelInfo[] = [
  {
    key: "anthropic::claude-opus-4-8",
    id: "claude-opus-4-8",
    display_name: "Claude Opus 4.8",
    provider: "anthropic",
    vendor: null,
    verification: "verified",
    available: false,
    context_window: 1_000_000,
    max_output_tokens: 128_000,
    input_modalities: ["text"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: [],
    multimodal: false,
    recommended: true,
  },
  {
    key: "openai::gpt-4o",
    id: "gpt-4o",
    display_name: "GPT-4o",
    provider: "openai",
    vendor: null,
    verification: "verified",
    available: true,
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text"],
    supports_reasoning: false,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: [],
    multimodal: false,
    recommended: true,
  },
  {
    key: "openai::gpt-4o-mini",
    id: "gpt-4o-mini",
    display_name: "GPT-4o mini",
    provider: "openai",
    vendor: null,
    verification: "verified",
    available: true,
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text"],
    supports_reasoning: false,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: [],
    multimodal: false,
    recommended: true,
  },
  {
    key: "openai::gpt-no-schema",
    id: "gpt-no-schema",
    display_name: "GPT No Schema",
    provider: "openai",
    vendor: null,
    verification: "unverified",
    available: true,
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text"],
    supports_reasoning: false,
    supports_tools: true,
    supports_structured_output: false,
    reasoning_efforts: [],
    multimodal: false,
    recommended: false,
  },
  {
    key: "together::moonshotai/Kimi-K3",
    id: "moonshotai/Kimi-K3",
    display_name: "Kimi K3",
    provider: "together",
    vendor: null,
    verification: "unverified",
    available: true,
    context_window: 1_000_000,
    max_output_tokens: 32_768,
    input_modalities: ["text", "image"],
    supports_reasoning: false,
    supports_tools: false,
    supports_structured_output: true,
    reasoning_efforts: [],
    multimodal: true,
    recommended: false,
  },
];

const roles: ModelRoleInfo[] = [
  // A legacy bare id, which the panel migrates to its provider-qualified key.
  { role: "chat", selection: "gpt-4o", resolved_key: "openai::gpt-4o" },
  { role: "utility", selection: null, resolved_key: "openai::gpt-4o-mini" },
];

// The settings document behind the panel's prompt-cache-retention read.
const runtimeSettings: RuntimeSettings = {
  model: null,
  has_api_key: false,
  chat_defaults: {
    model: null,
    reasoning_effort: null,
    permission_mode: null,
    network_policy: null,
  },
  max_active_background_agents: 5,
  sandbox_agent_checkin_steps: 100,
  sandbox_agent_error_checkin: 5,
  compaction: {
    threshold_fraction: 0.75,
    target_fraction: 0.25,
    min_threshold_tokens: 50000,
    protect_recent_messages: 5,
  },
  model_visibility_overrides: {},
  prompt_cache_retention: "five_minutes",
  computer_use_enabled: true,
  code_turn_recaps_enabled: true,
  rewrite_closing_messages: false,
};

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("ModelsPanel", () => {
  it("picks a model per role and names what automatic resolves to", async () => {
    const listModels = vi.fn().mockResolvedValue({ models, roles });
    const putModelRole = vi
      .fn()
      .mockImplementation(async (role: string, selection: string | null) => ({
        role,
        selection,
        resolved_key: selection ?? "openai::gpt-4o-mini",
      }));
    const onChanged = vi.fn();
    const getSettings = vi.fn().mockResolvedValue(runtimeSettings);
    const client = {
      listModels,
      putModelRole,
      getSettings,
    } as unknown as ApiClient;
    const user = userEvent.setup();

    render(
      <ModelsPanel client={client} models={models} onChanged={onChanged} />,
    );

    // The chat row opens on the provider owning its saved (legacy) id, resolved
    // to that provider's canonical model.
    const chatProvider = await screen.findByRole("combobox", {
      name: "Work provider",
    });
    await waitFor(() => expect(chatProvider).toHaveTextContent("OpenAI"));
    expect(
      screen.getByRole("combobox", { name: "Work model" }),
    ).toHaveTextContent("GPT-4o");
    await user.click(screen.getByRole("combobox", { name: "Work model" }));
    expect(
      screen.getByRole("option", {
        name: "GPT No Schema — 128k context",
      }),
    ).toBeInTheDocument();
    await user.keyboard("{Escape}");

    // The automatic choice says which model it lands on, per role.
    const utilityModel = screen.getByRole("combobox", {
      name: "Background work model",
    });
    expect(utilityModel).toHaveTextContent("Automatic — GPT-4o mini");

    // Switching a provider alone must not persist anything, and that provider's
    // only model cannot be chosen while it is unavailable.
    const utilityProvider = screen.getByRole("combobox", {
      name: "Background work provider",
    });
    await user.click(utilityProvider);
    await user.click(
      screen.getByRole("option", { name: "Anthropic — unavailable" }),
    );
    expect(putModelRole).not.toHaveBeenCalled();
    await user.click(utilityModel);
    expect(screen.queryByRole("option", { name: /GPT No Schema/ })).toBeNull();
    expect(
      screen.getByRole("option", {
        name: "Claude Opus 4.8 — 1M context — unavailable",
      }),
    ).toHaveAttribute("aria-disabled", "true");
    await user.keyboard("{Escape}");

    // Chat-only routing is independent from the strict structured-output
    // contract: Kimi K3 remains available for background work.
    await user.click(utilityProvider);
    await user.click(screen.getByRole("option", { name: "Together AI" }));
    await user.click(utilityModel);
    expect(
      screen.getByRole("option", {
        name: "Kimi K3 — 1M context — conversation only",
      }),
    ).toBeInTheDocument();
    await user.keyboard("{Escape}");

    // Choosing a model for one role pins that role.
    await user.click(utilityProvider);
    await user.click(screen.getByRole("option", { name: "OpenAI" }));
    await user.click(utilityModel);
    await user.click(
      screen.getByRole("option", { name: "GPT-4o mini — 128k context" }),
    );
    await waitFor(() =>
      expect(putModelRole).toHaveBeenCalledWith(
        "utility",
        "openai::gpt-4o-mini",
      ),
    );
    expect(onChanged).toHaveBeenCalled();

    // Back to automatic clears the pin rather than writing a model.
    await user.click(utilityModel);
    await user.click(
      screen.getByRole("option", { name: "Automatic — GPT-4o mini" }),
    );
    await waitFor(() =>
      expect(putModelRole).toHaveBeenCalledWith("utility", null),
    );
  });
});

function gatewayModel(
  id: string,
  displayName: string,
  contextWindow: number,
): ModelInfo {
  return {
    key: `model_gateway::${id}`,
    id,
    display_name: displayName,
    provider: "model_gateway",
    vendor: null,
    verification: "unverified",
    available: true,
    context_window: contextWindow,
    max_output_tokens: 8_192,
    input_modalities: ["text"],
    supports_reasoning: false,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: [],
    multimodal: false,
    recommended: false,
  };
}

// A managed catalog: the locked-out BYOK registry row is still reported by
// the server, and must not surface as a choice; the gateway rows are the
// entitled list.
const managedModels: ModelInfo[] = [
  models[0],
  gatewayModel("gw-flagship", "Gateway Flagship", 1_000_000),
  gatewayModel("gw-haiku", "Gateway Haiku", 200_000),
  {
    ...gatewayModel("moonshotai/Kimi-K3", "Kimi K3", 1_000_000),
    vendor: "together",
    supports_tools: false,
    supports_structured_output: true,
  },
];

// A BYOK chat pin carried in from before the profile was managed: the server
// reports it stored but resolves the role to a gateway model.
const managedRoles: ModelRoleInfo[] = [
  {
    role: "chat",
    selection: "anthropic::claude-opus-4-8",
    resolved_key: "model_gateway::gw-flagship",
  },
  { role: "utility", selection: null, resolved_key: "model_gateway::gw-haiku" },
];

describe("ModelsPanel under managed policy", () => {
  it("offers one flat entitled list per role and reads a dead pin as automatic", async () => {
    const listModels = vi
      .fn()
      .mockResolvedValue({ models: managedModels, roles: managedRoles });
    const putModelRole = vi
      .fn()
      .mockImplementation(async (role: string, selection: string | null) => ({
        role,
        selection,
        resolved_key: selection ?? "model_gateway::gw-flagship",
      }));
    const getSettings = vi.fn().mockResolvedValue(runtimeSettings);
    const client = {
      listModels,
      putModelRole,
      getSettings,
    } as unknown as ApiClient;
    const user = userEvent.setup();

    render(<ModelsPanel client={client} models={managedModels} managed />);

    // No provider step: there is exactly one provider, so each role is a
    // single flat picker.
    const chatModel = await screen.findByRole("combobox", {
      name: "Work model",
    });
    expect(
      screen.queryByRole("combobox", { name: "Work provider" }),
    ).toBeNull();
    expect(
      screen.queryByRole("combobox", { name: "Background work provider" }),
    ).toBeNull();

    // The stored BYOK pin is not gateway-served, so the role reads as the
    // automatic pick the server resolved it to — not a dead selection — and
    // says the pin is kept for a return to the open experience.
    expect(chatModel).toHaveTextContent("Automatic — Gateway Flagship");
    expect(
      screen.getByText(
        /Your previous Anthropic selection is kept and restored/,
      ),
    ).toBeInTheDocument();

    // The list is the entitled models, nothing else: no BYOK row, not even a
    // disabled one, because the reader cannot fix its unavailability here.
    await user.click(
      screen.getByRole("combobox", { name: "Background work model" }),
    );
    expect(
      screen.getByRole("option", { name: "Automatic — Gateway Haiku" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: "Gateway Flagship — 1M context" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("option", {
        name: "Kimi K3 — 1M context — conversation only",
      }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /Claude Opus/ })).toBeNull();

    await user.click(screen.getByRole("option", { name: /^Gateway Haiku/ }));
    await waitFor(() =>
      expect(putModelRole).toHaveBeenCalledWith(
        "utility",
        "model_gateway::gw-haiku",
      ),
    );

    // The automatic entry is a real choice while a dead pin exists: picking
    // it persists null, so automatic-by-reroute can become genuinely
    // automatic instead of the trigger already claiming the state.
    await user.click(chatModel);
    await user.click(
      screen.getByRole("option", {
        name: "Automatic — Gateway Flagship (clears your previous Claude Opus 4.8 pin)",
      }),
    );
    await waitFor(() =>
      expect(putModelRole).toHaveBeenCalledWith("chat", null),
    );
    // Cleared, the pin notice goes and automatic is simply the value.
    expect(
      screen.queryByText(/Your previous Anthropic selection is kept/),
    ).toBeNull();
    expect(chatModel).toHaveTextContent("Automatic — Gateway Flagship");
  });

  it("shows a completed model sync without a manual refresh", async () => {
    const unsynced = {
      models: [],
      roles: [
        { role: "chat", selection: null, resolved_key: null },
        { role: "utility", selection: null, resolved_key: null },
      ],
    };
    const listModels = vi
      .fn()
      .mockResolvedValueOnce(unsynced)
      .mockResolvedValue({ models: managedModels, roles: managedRoles });
    const getSettings = vi.fn().mockResolvedValue(runtimeSettings);
    const client = { listModels, getSettings } as unknown as ApiClient;

    vi.useFakeTimers();
    render(<ModelsPanel client={client} models={[]} managed />);
    // Flush the initial load: nothing synced yet.
    await act(async () => {});
    expect(screen.getByText(/has not synced any models/)).toBeInTheDocument();

    // The watch picks up the completed sync on its next tick.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_100);
    });
    expect(screen.queryByText(/has not synced any models/)).toBeNull();
    expect(
      screen.getByRole("combobox", { name: "Work model" }),
    ).toHaveTextContent("Automatic — Gateway Flagship");
  });
});
