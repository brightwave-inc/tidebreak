// @vitest-environment jsdom

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
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
