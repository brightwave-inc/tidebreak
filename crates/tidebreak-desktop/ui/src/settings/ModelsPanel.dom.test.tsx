// @vitest-environment jsdom

import { act, cleanup, render, screen } from "@testing-library/react";
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
  harness_update_channel: "pinned",
  git_source_control: {
    auto_rename_branches: true,
    branch_prefix_mode: "account",
    account_prefix: "alex/",
    effective_branch_prefix: "alex/",
  },
  memory: { enabled: true, capture_enabled: false, capture_ready: false },
};

afterEach(() => {
  cleanup();
  vi.useRealTimers();
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
