// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  // A legacy bare id, which the view migrates to its provider-qualified key.
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

    render(
      <ModelsPanel client={client} models={models} onChanged={onChanged} />,
    );

    // Two rows, each a provider select followed by a model select.
    await screen.findAllByLabelText("Provider");
    const [chatProvider, chatModel, utilityProvider, utilityModel] =
      screen.getAllByRole("combobox");

    // Chat opens on the provider that owns its saved (legacy) selection.
    await waitFor(() => expect(chatProvider).toHaveValue("openai"));
    expect(chatModel).toHaveValue("openai::gpt-4o");

    // The automatic option says which model it lands on, per role.
    expect(
      screen.getByRole("option", { name: "Automatic — GPT-4o mini" }),
    ).toBeInTheDocument();

    // Switching provider alone must never save.
    fireEvent.change(utilityProvider, { target: { value: "anthropic" } });
    expect(putModelRole).not.toHaveBeenCalled();
    expect(
      screen.getByRole("option", { name: "Claude Opus 4.8 — unavailable" }),
    ).toBeDisabled();

    fireEvent.change(utilityProvider, { target: { value: "openai" } });
    fireEvent.change(utilityModel, {
      target: { value: "openai::gpt-4o-mini" },
    });
    await waitFor(() =>
      expect(putModelRole).toHaveBeenCalledWith("utility", "openai::gpt-4o-mini"),
    );
    expect(onChanged).toHaveBeenCalled();

    // Back to automatic clears the pin rather than writing a model.
    fireEvent.change(utilityModel, { target: { value: "" } });
    await waitFor(() =>
      expect(putModelRole).toHaveBeenCalledWith("utility", null),
    );
  });
});
