// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";

import { ModelMenu } from "./ModelMenu";
import type { ModelInfo } from "./api";

vi.mock("sonner", () => ({ toast: { warning: vi.fn() } }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const MODELS: ModelInfo[] = [
  {
    key: "anthropic::claude-sonnet-4",
    id: "claude-sonnet-4",
    display_name: "Claude Sonnet 4",
    provider: "anthropic",
    vendor: null,
    verification: "verified",
    context_window: 200_000,
    max_output_tokens: 64_000,
    input_modalities: ["text"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "high"],
    multimodal: false,
    available: true,
    recommended: true,
  },
  {
    key: "anthropic::claude-haiku-4",
    id: "claude-haiku-4",
    display_name: "Claude Haiku 4",
    provider: "anthropic",
    vendor: null,
    verification: "verified",
    context_window: 200_000,
    max_output_tokens: 64_000,
    input_modalities: ["text"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "high"],
    multimodal: false,
    available: true,
    recommended: false,
  },
  {
    key: "openai::gpt-5",
    id: "gpt-5",
    display_name: "GPT-5",
    provider: "openai",
    vendor: null,
    verification: "verified",
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "high"],
    multimodal: false,
    available: true,
    recommended: false,
  },
];

it("warns only when a model choice switches providers", async () => {
  const onChange = vi.fn();
  render(
    <ModelMenu
      models={MODELS}
      value="anthropic::claude-sonnet-4"
      onSetUpProvider={() => {}}
      onChange={onChange}
    />,
  );

  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Model: Claude Sonnet 4" }));
  await user.click(screen.getByRole("menuitem", { name: "Claude Haiku 4" }));
  expect(toast.warning).not.toHaveBeenCalled();

  await user.click(screen.getByRole("button", { name: "Model: Claude Sonnet 4" }));
  await user.click(screen.getByRole("tab", { name: "OpenAI" }));
  await user.click(screen.getByRole("menuitem", { name: "GPT-5" }));

  expect(onChange).toHaveBeenLastCalledWith("openai::gpt-5");
  expect(toast.warning).toHaveBeenCalledWith("Prompt cache may not be reused", {
    description:
      "This change may prevent prompt cache reuse, increasing cost and latency on the next turn.",
  });
});
