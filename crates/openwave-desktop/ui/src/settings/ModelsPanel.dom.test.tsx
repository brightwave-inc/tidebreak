// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ModelInfo, ModelRoleInfo } from "../api";
import { ModelsPanel } from "./ModelsPanel";

const models: ModelInfo[] = [
  {
    key: "anthropic::claude-opus-4-8",
    id: "claude-opus-4-8",
    display_name: "Claude Opus 4.8",
    provider: "anthropic",
    available: false,
    context_window: 1_000_000,
    max_output_tokens: 128_000,
    input_modalities: ["text"],
    supports_reasoning: true,
    reasoning_efforts: [],
    multimodal: false,
  },
  {
    key: "openai::gpt-4o",
    id: "gpt-4o",
    display_name: "GPT-4o",
    provider: "openai",
    available: true,
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text"],
    supports_reasoning: false,
    reasoning_efforts: [],
    multimodal: false,
  },
  {
    key: "openai::gpt-4o-mini",
    id: "gpt-4o-mini",
    display_name: "GPT-4o mini",
    provider: "openai",
    available: true,
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text"],
    supports_reasoning: false,
    reasoning_efforts: [],
    multimodal: false,
  },
];

const roles: ModelRoleInfo[] = [
  // A legacy bare id, which the panel migrates to its provider-qualified key.
  { role: "chat", selection: "gpt-4o", resolved_key: "openai::gpt-4o" },
  { role: "utility", selection: null, resolved_key: "openai::gpt-4o-mini" },
];

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
    const client = { listModels, putModelRole } as unknown as ApiClient;
    const user = userEvent.setup();

    render(
      <ModelsPanel client={client} models={models} onChanged={onChanged} />,
    );

    // The chat row opens on the provider owning its saved (legacy) id, resolved
    // to that provider's canonical model.
    const chatProvider = await screen.findByRole("combobox", {
      name: "Chat provider",
    });
    await waitFor(() => expect(chatProvider).toHaveTextContent("OpenAI"));
    expect(
      screen.getByRole("combobox", { name: "Chat model" }),
    ).toHaveTextContent("GPT-4o");

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
    expect(
      screen.getByRole("option", { name: "Claude Opus 4.8 — unavailable" }),
    ).toHaveAttribute("aria-disabled", "true");
    await user.keyboard("{Escape}");

    // Choosing a model for one role pins that role.
    await user.click(utilityProvider);
    await user.click(screen.getByRole("option", { name: "OpenAI" }));
    await user.click(utilityModel);
    await user.click(screen.getByRole("option", { name: "GPT-4o mini" }));
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
    available: true,
    context_window: contextWindow,
    max_output_tokens: 8_192,
    input_modalities: ["text"],
    supports_reasoning: false,
    reasoning_efforts: [],
    multimodal: false,
  };
}

// A managed catalog: the locked-out BYOK registry row is still reported by
// the server, and must not surface as a choice; the gateway rows are the
// entitled list.
const managedModels: ModelInfo[] = [
  models[0],
  gatewayModel("gw-flagship", "Gateway Flagship", 1_000_000),
  gatewayModel("gw-haiku", "Gateway Haiku", 200_000),
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
    const putModelRole = vi.fn().mockResolvedValue({
      role: "utility",
      selection: "model_gateway::gw-haiku",
      resolved_key: "model_gateway::gw-haiku",
    });
    const client = { listModels, putModelRole } as unknown as ApiClient;
    const user = userEvent.setup();

    render(<ModelsPanel client={client} models={managedModels} managed />);

    // No provider step: there is exactly one provider, so each role is a
    // single flat picker.
    const chatModel = await screen.findByRole("combobox", {
      name: "Chat model",
    });
    expect(
      screen.queryByRole("combobox", { name: "Chat provider" }),
    ).toBeNull();
    expect(
      screen.queryByRole("combobox", { name: "Background work provider" }),
    ).toBeNull();

    // The stored BYOK pin is not gateway-served, so the role reads as the
    // automatic pick the server resolved it to — not a dead selection.
    expect(chatModel).toHaveTextContent("Automatic — Gateway Flagship");

    // The list is the entitled models, nothing else: no BYOK row, not even a
    // disabled one, because the reader cannot fix its unavailability here.
    await user.click(
      screen.getByRole("combobox", { name: "Background work model" }),
    );
    expect(
      screen.getByRole("option", { name: "Automatic — Gateway Haiku" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: "Gateway Flagship" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /Claude Opus/ })).toBeNull();

    await user.click(screen.getByRole("option", { name: "Gateway Haiku" }));
    await waitFor(() =>
      expect(putModelRole).toHaveBeenCalledWith(
        "utility",
        "model_gateway::gw-haiku",
      ),
    );
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
    const client = { listModels } as unknown as ApiClient;

    vi.useFakeTimers();
    render(<ModelsPanel client={client} models={[]} managed />);
    // Flush the initial load: nothing synced yet.
    await act(async () => {});
    expect(screen.getByText(/has not synced any models/)).toBeInTheDocument();

    // The watch picks up the completed sync on its next tick.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_100);
    });
    expect(screen.queryByText(/has not synced any models/)).toBeNull();
    expect(
      screen.getByRole("combobox", { name: "Chat model" }),
    ).toHaveTextContent("Automatic — Gateway Flagship");
  });
});
