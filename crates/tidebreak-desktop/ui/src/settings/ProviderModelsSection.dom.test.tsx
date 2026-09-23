// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  HttpError,
  type ApiClient,
  type CustomModelConfig,
  type ModelInfo,
  type ProviderInfo,
} from "../api";
import {
  discoveredAnthropicModels,
  providerInfoFixture,
} from "../stories/fixtures";
import { ProviderModelsSection } from "./ProviderModelsSection";

const saved: CustomModelConfig = {
  id: "claude-sonnet-5-5",
  display_name: "Claude Sonnet 5.5",
  context_window: 1_000_000,
  max_output_tokens: 128_000,
  input_modalities: ["text", "image"],
  supports_reasoning: true,
  reasoning_efforts: ["low", "high"],
  supports_tools: true,
};

function anthropic(overrides: Partial<ProviderInfo> = {}): ProviderInfo {
  return providerInfoFixture("anthropic", {
    enabled: true,
    has_credential: true,
    models: [saved],
    ...overrides,
  });
}

function catalogRow(id: string, name: string): ModelInfo {
  return {
    key: `anthropic::${id}`,
    id,
    display_name: name,
    provider: "anthropic",
    vendor: null,
    verification: "verified",
    available: true,
    context_window: 1_000_000,
    max_output_tokens: 128_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "medium", "high", "xhigh", "max"],
    multimodal: true,
    recommended: true,
  };
}

const catalog = [
  catalogRow("claude-opus-5-5", "Claude Opus 5.5"),
  catalogRow("claude-sonnet-5", "Claude Sonnet 5"),
  // The saved custom row, as the catalog lists it beside the built-in ones.
  catalogRow("claude-sonnet-5-5", "Claude Sonnet 5.5"),
];

function renderSection(
  client: Partial<ApiClient>,
  info: ProviderInfo = anthropic(),
) {
  const onChanged = vi.fn();
  render(
    <ProviderModelsSection
      info={info}
      catalogModels={catalog}
      client={client as ApiClient}
      onChanged={onChanged}
    />,
  );
  return { onChanged };
}

afterEach(cleanup);

describe("ProviderModelsSection", () => {
  it("lists built-in models by name and custom models with their facts", () => {
    renderSection({});

    expect(
      screen.getByText("Built in: Claude Opus 5.5, Claude Sonnet 5"),
    ).toBeInTheDocument();
    const list = screen.getByRole("list", {
      name: "Anthropic custom models",
    });
    expect(within(list).getByText("Claude Sonnet 5.5")).toBeInTheDocument();
    expect(within(list).getByText("claude-sonnet-5-5")).toBeInTheDocument();
    expect(
      within(list).getByText(
        "1M context · 128k output · Images · Reasoning, Low to High",
      ),
    ).toBeInTheDocument();
    expect(
      within(list).getByRole("button", { name: "Edit Claude Sonnet 5.5" }),
    ).toBeInTheDocument();
    expect(
      within(list).getByRole("button", { name: "Remove Claude Sonnet 5.5" }),
    ).toBeInTheDocument();
  });

  it("adds a model with images, the reasoning levels it takes, and tools off", async () => {
    const putProvider = vi.fn().mockResolvedValue(anthropic());
    const { onChanged } = renderSection({ putProvider });

    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    const dialog = await screen.findByRole("dialog", {
      name: "Add a model to Anthropic",
    });
    const field = (label: string) => within(dialog).getByLabelText(label);
    fireEvent.change(field("Model ID"), {
      target: { value: "claude-haiku-5-5" },
    });
    fireEvent.change(field("Context window"), {
      target: { value: "400000" },
    });
    fireEvent.change(field("Max output"), { target: { value: "64000" } });
    fireEvent.click(
      within(dialog).getByRole("switch", { name: "Accepts images" }),
    );
    fireEvent.click(
      within(dialog).getByRole("switch", { name: "Supports tools" }),
    );
    fireEvent.click(within(dialog).getByRole("switch", { name: "Reasons" }));
    // Anthropic sends no "Off" level, so the form never offers one.
    expect(
      within(dialog).queryByRole("checkbox", { name: "Off" }),
    ).not.toBeInTheDocument();
    // Picked out of order; saved in scale order.
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "Max" }));
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "Low" }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Add model" }));

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("anthropic", {
        models: [
          saved,
          {
            id: "claude-haiku-5-5",
            context_window: 400_000,
            max_output_tokens: 64_000,
            input_modalities: ["text", "image"],
            supports_reasoning: true,
            reasoning_efforts: ["low", "max"],
            supports_tools: false,
          },
        ],
      }),
    );
    expect(onChanged).toHaveBeenCalled();
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
  });

  it("points at each field that is wrong before saving anything", async () => {
    const putProvider = vi.fn();
    renderSection({ putProvider });

    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    const dialog = await screen.findByRole("dialog");
    const submit = () =>
      fireEvent.click(
        within(dialog).getByRole("button", { name: "Add model" }),
      );

    submit();
    expect(
      within(dialog).getByText(
        "Enter the model ID exactly as the provider's API spells it.",
      ),
    ).toBeInTheDocument();

    fireEvent.change(within(dialog).getByLabelText("Model ID"), {
      target: { value: "claude-opus-5-5" },
    });
    expect(
      within(dialog).getByText("This model is already built in."),
    ).toBeInTheDocument();

    fireEvent.change(within(dialog).getByLabelText("Model ID"), {
      target: { value: "claude-sonnet-5-5" },
    });
    expect(
      within(dialog).getByText("You already added this model."),
    ).toBeInTheDocument();

    fireEvent.change(within(dialog).getByLabelText("Model ID"), {
      target: { value: "claude-next" },
    });
    fireEvent.change(within(dialog).getByLabelText("Context window"), {
      target: { value: "8000" },
    });
    fireEvent.change(within(dialog).getByLabelText("Max output"), {
      target: { value: "9000" },
    });
    expect(
      within(dialog).getByText("Max output cannot exceed the context window."),
    ).toBeInTheDocument();

    fireEvent.change(within(dialog).getByLabelText("Context window"), {
      target: { value: "100" },
    });
    expect(
      within(dialog).getByText("Enter a whole number from 1,024 to 4,000,000."),
    ).toBeInTheDocument();
    submit();
    expect(putProvider).not.toHaveBeenCalled();
  });

  it("edits a saved model in place", async () => {
    const putProvider = vi.fn().mockResolvedValue(anthropic());
    renderSection({ putProvider });

    fireEvent.click(
      screen.getByRole("button", { name: "Edit Claude Sonnet 5.5" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Edit Claude Sonnet 5.5",
    });
    expect(within(dialog).getByLabelText("Model ID")).toHaveValue(
      "claude-sonnet-5-5",
    );
    expect(within(dialog).getByRole("checkbox", { name: "Low" })).toBeChecked();
    fireEvent.change(within(dialog).getByLabelText("Display name"), {
      target: { value: "Sonnet 5.5 preview" },
    });
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Save changes" }),
    );

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("anthropic", {
        models: [{ ...saved, display_name: "Sonnet 5.5 preview" }],
      }),
    );
  });

  it("keeps the dialog open with the server's reason when a save is refused", async () => {
    const putProvider = vi
      .fn()
      .mockRejectedValue(
        new HttpError(
          400,
          "400: configured model `claude-next` lists reasoning effort `none`, which the anthropic route does not send",
        ),
      );
    renderSection({ putProvider });

    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Model ID"), {
      target: { value: "claude-next" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add model" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "configured model `claude-next` lists reasoning effort `none`",
    );
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("removes a model after the reader confirms", async () => {
    const putProvider = vi.fn().mockResolvedValue(anthropic({ models: [] }));
    renderSection({ putProvider });

    fireEvent.click(
      screen.getByRole("button", { name: "Remove Claude Sonnet 5.5" }),
    );
    const confirm = await screen.findByRole("alertdialog");
    expect(confirm).toHaveTextContent("Remove Claude Sonnet 5.5?");
    fireEvent.click(
      within(confirm).getByRole("button", { name: "Remove model" }),
    );

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("anthropic", { models: [] }),
    );
  });

  it("drops a stored reasoning level the provider no longer takes when it saves", async () => {
    const putProvider = vi.fn().mockResolvedValue(anthropic());
    const stale: CustomModelConfig = {
      ...saved,
      // Written before Anthropic's levels were checked; Anthropic sends no
      // "none".
      reasoning_efforts: ["none", "low"],
    };
    renderSection({ putProvider }, anthropic({ models: [stale] }));

    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Model ID"), {
      target: { value: "claude-next" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add model" }));

    await waitFor(() => expect(putProvider).toHaveBeenCalled());
    const [, body] = putProvider.mock.calls[0];
    expect(body.models[0].reasoning_efforts).toEqual(["low"]);
  });

  it("finds models, marks the known ones, and adds the picked ones with their reported limits", async () => {
    const discoverProviderModels = vi.fn().mockResolvedValue({
      provider: "anthropic",
      models: discoveredAnthropicModels,
    });
    const putProvider = vi.fn().mockResolvedValue(anthropic());
    renderSection({ discoverProviderModels, putProvider });

    fireEvent.click(screen.getByRole("button", { name: "Find models" }));
    const dialog = await screen.findByRole("dialog", {
      name: "Find Anthropic models",
    });
    const list = await within(dialog).findByRole("list", {
      name: "Anthropic models",
    });
    expect(discoverProviderModels).toHaveBeenCalledWith("anthropic");

    // Built-in and added rows cannot be picked twice.
    const opus = within(list).getByRole("checkbox", {
      name: "Add Claude Opus 5.5",
    });
    expect(opus).toBeDisabled();
    expect(opus).toBeChecked();
    expect(
      within(list).getByRole("checkbox", { name: "Add Claude Sonnet 5.5" }),
    ).toBeDisabled();
    expect(within(list).getAllByText("Built in")).toHaveLength(3);
    expect(within(list).getAllByText("Added")).toHaveLength(1);

    const review = within(dialog).getByRole("button", {
      name: "Review models",
    });
    expect(review).toBeDisabled();
    fireEvent.click(
      within(list).getByRole("checkbox", { name: "Add Claude Haiku 5.5" }),
    );
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Review 1 model" }),
    );

    // The review step starts from what the provider reported, and stays
    // editable.
    await within(dialog).findByText("Check 1 model before adding");
    expect(within(dialog).getByText("claude-haiku-5-5")).toBeInTheDocument();
    expect(within(dialog).getByLabelText("Context window")).toHaveValue(
      "400,000",
    );
    fireEvent.change(within(dialog).getByLabelText("Max output"), {
      target: { value: "32000" },
    });
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Add 1 model" }),
    );

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("anthropic", {
        models: [
          saved,
          {
            id: "claude-haiku-5-5",
            display_name: "Claude Haiku 5.5",
            context_window: 400_000,
            max_output_tokens: 32_000,
            input_modalities: ["text", "image"],
            supports_reasoning: true,
            reasoning_efforts: ["low", "medium", "high", "xhigh", "max"],
            supports_tools: true,
          },
        ],
      }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
  });

  it("says plainly why finding models failed and tries again on request", async () => {
    const discoverProviderModels = vi
      .fn()
      .mockRejectedValueOnce(
        new HttpError(
          502,
          "502: Anthropic rejected the saved API key (HTTP 401). Save a valid key and try again.",
          "provider_credential_rejected",
        ),
      )
      .mockResolvedValueOnce({ provider: "anthropic", models: [] });
    renderSection({ discoverProviderModels });

    fireEvent.click(screen.getByRole("button", { name: "Find models" }));
    const dialog = await screen.findByRole("dialog");
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent(
      "Anthropic rejected the saved API key (HTTP 401). Save a valid key and try again.",
    );
    // The status prefix the client adds is not part of the message.
    expect(alert.textContent).not.toMatch(/^502/);

    fireEvent.click(within(dialog).getByRole("button", { name: "Try again" }));
    expect(
      await within(dialog).findByText(
        "Anthropic listed no chat models for this key.",
      ),
    ).toBeInTheDocument();
    expect(discoverProviderModels).toHaveBeenCalledTimes(2);
  });
});
