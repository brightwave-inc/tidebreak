// @vitest-environment jsdom

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { ApiClient, ModelInfo } from "../api";
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
];

describe("ModelsPanel", () => {
  it("shows provider-first typed selection and never saves a provider alone", async () => {
    const getSettings = vi.fn().mockResolvedValue({
      model: "gpt-4o", // legacy bare id migrates in the view
      has_api_key: true,
    });
    const putSettings = vi.fn().mockResolvedValue({
      model: null,
      has_api_key: true,
    });
    const client = { getSettings, putSettings } as unknown as ApiClient;
    const user = userEvent.setup();

    render(<ModelsPanel client={client} models={models} />);

    // The saved bare id resolves to its provider and canonical model.
    const provider = await screen.findByRole("combobox", { name: "Provider" });
    await waitFor(() => expect(provider).toHaveTextContent("OpenAI"));
    expect(
      screen.getByRole("combobox", { name: "Model" }),
    ).toHaveTextContent("GPT-4o");

    // Switching provider alone must not persist anything.
    await user.click(provider);
    await user.click(
      screen.getByRole("option", { name: "Anthropic — unavailable" }),
    );
    expect(putSettings).not.toHaveBeenCalled();

    // That provider's only model is unavailable and cannot be chosen.
    await user.click(screen.getByRole("combobox", { name: "Model" }));
    expect(
      screen.getByRole("option", { name: "Claude Opus 4.8 — unavailable" }),
    ).toHaveAttribute("aria-disabled", "true");
    await user.keyboard("{Escape}");

    // Choosing a model — here the server default over the migrated GPT-4o —
    // is what persists.
    await user.click(provider);
    await user.click(screen.getByRole("option", { name: "OpenAI" }));
    await user.click(screen.getByRole("combobox", { name: "Model" }));
    await user.click(screen.getByRole("option", { name: "Server default" }));
    await waitFor(() =>
      expect(putSettings).toHaveBeenCalledWith({ model: null }),
    );
  });
});
