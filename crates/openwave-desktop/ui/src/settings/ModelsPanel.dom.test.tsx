// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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

    render(<ModelsPanel client={client} models={models} />);

    const provider = await screen.findByLabelText("Provider");
    await waitFor(() => expect(provider).toHaveValue("openai"));
    const model = screen.getAllByRole("combobox")[1];
    expect(model).toHaveValue("openai::gpt-4o");
    expect(screen.getByText("GPT-4o")).toBeInTheDocument();

    fireEvent.change(provider, { target: { value: "anthropic" } });
    expect(putSettings).not.toHaveBeenCalled();
    const unavailable = screen.getByRole("option", {
      name: "Claude Opus 4.8 — unavailable",
    });
    expect(unavailable).toBeDisabled();

    fireEvent.change(model, {
      target: { value: "" },
    });
    await waitFor(() => expect(putSettings).toHaveBeenCalledWith({ model: null }));
  });
});
