import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn, userEvent, within } from "storybook/test";

import {
  HttpError,
  type ApiClient,
  type DiscoveredModel,
  type DiscoveredModels,
  type ModelInfo,
  type ProviderInfo,
} from "@/api";
import { CustomModelDialog } from "@/settings/CustomModelDialog";
import {
  draftFromConfig,
  emptyDraft,
  type ModelDraft,
} from "@/settings/customModels";
import { DiscoverModelsDialog } from "@/settings/DiscoverModelsDialog";
import { ProvidersPanel } from "@/settings/ProvidersPanel";
import {
  customReasoningEfforts,
  discoveredAnthropicModels,
  providerInfoFixture,
} from "./fixtures";
import { SettingsStoryHarness } from "./SettingsStoryHarness";

/** Dialogs portal to `document.body`, so stories query the whole page. */
const page = (canvasElement: HTMLElement) =>
  within(canvasElement.ownerDocument.body);

function catalogRow(
  provider: ModelInfo["provider"],
  id: string,
  name: string,
): ModelInfo {
  return {
    key: `${provider}::${id}`,
    id,
    display_name: name,
    provider,
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

const sonnetPreview = {
  id: "claude-sonnet-5-5",
  display_name: "Claude Sonnet 5.5",
  context_window: 1_000_000,
  max_output_tokens: 128_000,
  input_modalities: ["text", "image"],
  supports_reasoning: true,
  reasoning_efforts: ["low", "medium", "high", "xhigh", "max"],
  supports_tools: true,
} satisfies ProviderInfo["models"][number];

const fineTune = {
  id: "ft:gpt-6-luna:acme:support-triage:9xk2",
  display_name: "Support triage",
  context_window: 128_000,
  max_output_tokens: 16_384,
  input_modalities: ["text"],
  supports_reasoning: true,
  reasoning_efforts: ["none", "low", "medium"],
  supports_tools: false,
} satisfies ProviderInfo["models"][number];

const providers: ProviderInfo[] = [
  providerInfoFixture("anthropic", {
    enabled: true,
    has_credential: true,
    models: [sonnetPreview],
  }),
  providerInfoFixture("openai", {
    enabled: true,
    has_credential: true,
    auth_mode: "api_key",
    models: [fineTune],
  }),
  providerInfoFixture("gemini", { enabled: true, has_credential: true }),
  providerInfoFixture("xai"),
];

const catalog: ModelInfo[] = [
  catalogRow("anthropic", "claude-opus-5-5", "Claude Opus 5.5"),
  catalogRow("anthropic", "claude-fable-5-1", "Claude Fable 5.1"),
  catalogRow("anthropic", "claude-sonnet-5", "Claude Sonnet 5"),
  catalogRow("anthropic", "claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
  catalogRow("anthropic", "claude-sonnet-5-5", "Claude Sonnet 5.5"),
  catalogRow("openai", "gpt-5.6-sol", "GPT-5.6 Sol"),
  catalogRow("openai", "gpt-6-sol", "GPT-6 Sol"),
  catalogRow("openai", fineTune.id, "Support triage"),
  catalogRow("gemini", "gemini-3.8-flash", "Gemini 3.8 Flash"),
  catalogRow("gemini", "gemini-3.1-pro-preview", "Gemini 3.1 Pro Preview"),
  catalogRow("xai", "grok-4.7", "Grok 4.7"),
  catalogRow("xai", "grok-4.5", "Grok 4.5"),
];

type ProvidersStoryProps = { expand: ProviderInfo["kind"] };

function ProvidersStory({ expand }: ProvidersStoryProps) {
  return (
    <SettingsStoryHarness state="configured">
      {(client) => (
        <ProvidersPanel
          providers={providers}
          models={catalog}
          client={client}
          onChanged={fn()}
          expandProvider={expand}
        />
      )}
    </SettingsStoryHarness>
  );
}

/** A long OpenRouter-style listing, for the filter and the scroll. */
const longListing: DiscoveredModel[] = [
  ["anthropic/claude-opus-5.5", "Anthropic: Claude Opus 5.5", 1_000_000],
  ["anthropic/claude-sonnet-5", "Anthropic: Claude Sonnet 5", 1_000_000],
  ["deepseek/deepseek-v4.1-flash", "DeepSeek: V4.1 Flash", 1_000_000],
  ["google/gemini-3.8-flash", "Google: Gemini 3.8 Flash", 1_048_576],
  ["google/gemini-3.1-pro-preview", "Google: Gemini 3.1 Pro", 1_048_576],
  ["meta-llama/llama-4-maverick", "Meta: Llama 4 Maverick", 1_048_576],
  ["meta-llama/llama-3.3-70b-instruct", "Meta: Llama 3.3 70B", 131_072],
  ["mistralai/mistral-large-3", "Mistral: Large 3", 262_144],
  ["moonshotai/kimi-k3", "MoonshotAI: Kimi K3", 1_048_576],
  ["openai/gpt-6-sol", "OpenAI: GPT-6 Sol", 1_050_000],
  ["openai/gpt-6-luna", "OpenAI: GPT-6 Luna", 1_050_000],
  ["qwen/qwen3.8-max", "Qwen: Qwen3.8 Max", 1_000_000],
  ["x-ai/grok-4.7", "xAI: Grok 4.7", 500_000],
  ["z-ai/glm-5.3", "Z.ai: GLM 5.3", 1_048_575],
].map(([id, name, context], index) => ({
  id: id as string,
  display_name: name as string,
  context_window: context as number,
  max_output_tokens: index % 3 === 0 ? 64_000 : undefined,
  image_input: index % 2 === 0,
  supports_reasoning: index % 4 !== 1,
  supports_tools: index % 5 !== 2,
  built_in: false,
  added: index === 4,
}));

type DiscoverStoryProps = {
  kind: ProviderInfo["kind"];
  providerName: string;
  outcome: "loading" | "ready" | "empty" | "failed";
  listing?: DiscoveredModel[];
};

function discoveryClient({
  kind,
  outcome,
  listing = discoveredAnthropicModels,
}: DiscoverStoryProps): Pick<ApiClient, "discoverProviderModels"> {
  return {
    discoverProviderModels: (): Promise<DiscoveredModels> => {
      switch (outcome) {
        case "loading":
          return new Promise(() => undefined);
        case "failed":
          return Promise.reject(
            new HttpError(
              502,
              "502: Anthropic rejected the saved API key (HTTP 401). Save a valid key and try again.",
              "provider_credential_rejected",
            ),
          );
        case "empty":
          return Promise.resolve({ provider: kind, models: [] });
        case "ready":
          return Promise.resolve({ provider: kind, models: listing });
      }
    },
  };
}

function DiscoverStory(props: DiscoverStoryProps) {
  const [open, setOpen] = useState(true);
  const [client] = useState(() => discoveryClient(props));
  return (
    <DiscoverModelsDialog
      open={open}
      onOpenChange={setOpen}
      kind={props.kind}
      providerName={props.providerName}
      client={client}
      acceptedEfforts={customReasoningEfforts[props.kind]}
      existingCount={1}
      onAdd={async () => undefined}
    />
  );
}

type FormStoryProps = { mode: "add" | "edit"; initial: ModelDraft };

function FormStory({ mode, initial }: FormStoryProps) {
  const [open, setOpen] = useState(true);
  return (
    <CustomModelDialog
      open={open}
      onOpenChange={setOpen}
      mode={mode}
      providerName="Anthropic"
      initial={initial}
      acceptedEfforts={customReasoningEfforts.anthropic}
      takenIds={new Set(["claude-sonnet-5-5"])}
      builtInIds={new Set(["claude-opus-5-5", "claude-sonnet-5"])}
      onSave={async () => undefined}
    />
  );
}

const meta = {
  title: "Settings/Provider models",
  parameters: { layout: "fullscreen" },
} satisfies Meta;

export default meta;
type Story = StoryObj<typeof meta>;

/** An Anthropic card: built-in names, one custom model, and both actions. */
export const CustomModelsInACard: Story = {
  render: () => <ProvidersStory expand="anthropic" />,
};

export const CustomModelsInACardCompact: Story = {
  render: () => <ProvidersStory expand="anthropic" />,
  globals: { viewport: { value: "compact", isRotated: false } },
};

/** A fine-tune on OpenAI that reasons but never calls tools. */
export const ChatOnlyCustomModel: Story = {
  render: () => <ProvidersStory expand="openai" />,
};

/** Connected, with no custom models yet. */
export const NoCustomModels: Story = {
  render: () => <ProvidersStory expand="gemini" />,
};

/** No key saved: Find models waits and says why. */
export const FindModelsNeedsAKey: Story = {
  render: () => <ProvidersStory expand="xai" />,
};

export const AddModelForm: Story = {
  render: () => <FormStory mode="add" initial={emptyDraft()} />,
};

/** Submitted with an id already built in and an output above the window. */
export const AddModelFormErrors: Story = {
  render: () => (
    <FormStory
      mode="add"
      initial={{
        ...emptyDraft(),
        id: "claude-opus-5-5",
        contextWindow: "8000",
        maxOutputTokens: "9000",
      }}
    />
  ),
  play: async ({ canvasElement }) => {
    const dialog = await page(canvasElement).findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Add model" }),
    );
    await within(dialog).findByText("This model is already built in.");
  },
};

export const AddModelFormCompact: Story = {
  render: () => <FormStory mode="add" initial={emptyDraft()} />,
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const EditModelForm: Story = {
  render: () => (
    <FormStory mode="edit" initial={draftFromConfig(sonnetPreview)} />
  ),
};

export const FindModelsLoading: Story = {
  render: () => (
    <DiscoverStory
      kind="anthropic"
      providerName="Anthropic"
      outcome="loading"
    />
  ),
};

/** Built-in and added rows are marked and cannot be picked twice. */
export const FindModelsResults: Story = {
  render: () => (
    <DiscoverStory kind="anthropic" providerName="Anthropic" outcome="ready" />
  ),
  play: async ({ canvasElement }) => {
    const dialog = await page(canvasElement).findByRole("dialog");
    await userEvent.click(
      await within(dialog).findByRole("checkbox", {
        name: "Add Claude Haiku 5.5",
      }),
    );
  },
};

export const FindModelsResultsCompact: Story = {
  ...FindModelsResults,
  globals: { viewport: { value: "compact", isRotated: false } },
};

/** A long listing gets a filter, and the list scrolls inside the dialog. */
export const FindModelsLongListing: Story = {
  render: () => (
    <DiscoverStory
      kind="openrouter"
      providerName="OpenRouter"
      outcome="ready"
      listing={longListing}
    />
  ),
};

/** Two models picked; their reported limits are ready to adjust. */
export const FindModelsReview: Story = {
  render: () => (
    <DiscoverStory kind="anthropic" providerName="Anthropic" outcome="ready" />
  ),
  play: async ({ canvasElement }) => {
    const dialog = await page(canvasElement).findByRole("dialog");
    for (const name of ["Add Claude Haiku 5.5", "Add Claude Sonnet 3.7"]) {
      await userEvent.click(
        await within(dialog).findByRole("checkbox", { name }),
      );
    }
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Review 2 models" }),
    );
    await within(dialog).findByText("Check 2 models before adding");
  },
};

export const FindModelsReviewCompact: Story = {
  ...FindModelsReview,
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const FindModelsFailed: Story = {
  render: () => (
    <DiscoverStory kind="anthropic" providerName="Anthropic" outcome="failed" />
  ),
};

export const FindModelsEmpty: Story = {
  render: () => (
    <DiscoverStory kind="ollama" providerName="Ollama" outcome="empty" />
  ),
};
