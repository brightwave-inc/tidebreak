import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import type {
  ApiClient,
  ModelInfo,
  ProviderInfo,
  ProviderTestResult,
} from "@/api";
import { ProvidersPanel } from "@/settings/ProvidersPanel";
import { providerInfoFixture } from "./fixtures";
import { SettingsStoryHarness } from "./SettingsStoryHarness";

/** A time a few minutes before the story renders, so the card says "ago". */
function minutesAgo(minutes: number): string {
  return new Date(Date.now() - minutes * 60_000).toISOString();
}

function tested(
  outcome: ProviderTestResult["outcome"],
  message: string,
  extra: Partial<ProviderTestResult> = {},
): ProviderTestResult {
  return { outcome, message, tested_at: minutesAgo(4), ...extra };
}

const anthropicRejected = providerInfoFixture("anthropic", {
  enabled: true,
  has_credential: true,
  last_test: tested(
    "key_rejected",
    "Anthropic rejected the saved API key (HTTP 401). Save a valid key, then test again.",
    { status: 401 },
  ),
});

const openaiConnected = providerInfoFixture("openai", {
  enabled: true,
  has_credential: true,
  auth_mode: "api_key",
  last_test: tested("connected", "OpenAI accepted the saved key."),
});

const geminiLimited = providerInfoFixture("gemini", {
  enabled: true,
  has_credential: true,
  last_test: tested(
    "rate_limited",
    "Google Gemini is limiting requests (HTTP 429). Wait a minute, then test again.",
    { status: 429 },
  ),
});

const openaiDenied = providerInfoFixture("openai", {
  enabled: true,
  has_credential: true,
  auth_mode: "api_key",
  last_test: tested(
    "access_denied",
    "OpenAI refused access (HTTP 403). The key may lack a permission, or the account may be out of credits or restricted.",
    { status: 403 },
  ),
});

const xaiUntested = providerInfoFixture("xai", {
  enabled: true,
  has_credential: true,
});

const openrouterUnexpected = providerInfoFixture("openrouter", {
  enabled: true,
  has_credential: true,
  base_url: "https://openrouter.ai/api/v1",
  last_test: tested(
    "unexpected_answer",
    "OpenRouter answered with HTTP 503 instead of a model list.",
    { status: 503 },
  ),
});

const ollamaConnected = providerInfoFixture("ollama", {
  enabled: true,
  base_url: "http://127.0.0.1:11434/v1",
  last_test: tested(
    "connected",
    "Tidebreak reached Ollama. 3 models are pulled.",
    {
      model_count: 3,
    },
  ),
});

const compatibleUnreachable = providerInfoFixture("openai_compatible", {
  enabled: true,
  base_url: "http://127.0.0.1:1234/v1",
  last_test: tested(
    "unreachable",
    "Tidebreak could not reach the server at http://127.0.0.1:1234/v1. Check that it is running.",
  ),
});

/** A key saved for a server on this computer, sent over HTTP by consent. */
const compatibleWithKey = providerInfoFixture("openai_compatible", {
  enabled: true,
  has_credential: true,
  base_url: "http://127.0.0.1:1234/v1",
  allow_loopback_http: true,
  last_test: tested(
    "connected",
    "The server accepted the saved key. It lists 2 models.",
    { model_count: 2 },
  ),
});

const catalog: ModelInfo[] = [];

type ProviderConnectionStoryProps = {
  providers: ProviderInfo[];
  expand?: ProviderInfo["kind"];
  /** Replaces the harness's connection test, for the in-flight and refused states. */
  testProvider?: ApiClient["testProvider"];
};

function ProviderConnectionStory({
  providers,
  expand,
  testProvider,
}: ProviderConnectionStoryProps) {
  return (
    <SettingsStoryHarness state="configured">
      {(client) => (
        <ProvidersPanel
          providers={providers}
          models={catalog}
          client={
            testProvider
              ? (Object.assign(Object.create(client), {
                  testProvider,
                }) as ApiClient)
              : client
          }
          onChanged={fn()}
          expandProvider={expand}
        />
      )}
    </SettingsStoryHarness>
  );
}

const meta = {
  title: "Settings/Provider connection",
  component: ProviderConnectionStory,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof ProviderConnectionStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * Every badge says what that provider's last test found, not only that a key
 * is stored: a rejected key, a limited account, a stopped server, and one
 * card nobody has tested yet.
 */
export const Badges: Story = {
  args: {
    providers: [
      openaiConnected,
      anthropicRejected,
      xaiUntested,
      geminiLimited,
      openrouterUnexpected,
      ollamaConnected,
      compatibleUnreachable,
    ],
  },
};

/** A rejected key: the card leads with what the provider answered and when. */
export const KeyRejected: Story = {
  args: { providers: [anthropicRejected], expand: "anthropic" },
};

/** A local server that did not answer, with the address it tried. */
export const Unreachable: Story = {
  args: { providers: [compatibleUnreachable], expand: "openai_compatible" },
};

/**
 * HTTP 403: the key may lack a permission, or the account may be out of
 * credits. The card does not call the key invalid.
 */
export const AccessDenied: Story = {
  args: { providers: [openaiDenied], expand: "openai" },
};

/** A test in flight: the button says so until the provider answers. */
export const Testing: Story = {
  args: {
    providers: [xaiUntested],
    expand: "xai",
    testProvider: () => new Promise<never>(() => {}),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByRole("button", { name: "Test" }));
    await expect(
      await canvas.findByRole("button", { name: "Testing…" }),
    ).toBeDisabled();
  },
};

/**
 * Tidebreak could not start the test, here because the saved credential
 * could not be read, and the card says what to do instead of a verdict.
 */
export const TestCouldNotStart: Story = {
  args: {
    providers: [anthropicRejected],
    expand: "anthropic",
    testProvider: () =>
      Promise.reject(
        new Error(
          "The saved Anthropic credential could not be read. Save it again.",
        ),
      ),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByRole("button", { name: "Test" }));
    await expect(await canvas.findByText(/could not be read/)).toBeVisible();
  },
};

/** The provider is limiting requests: a warning, not a failure. */
export const RateLimited: Story = {
  args: { providers: [geminiLimited], expand: "gemini" },
};

/** Something answered, but not with a model list. */
export const UnexpectedAnswer: Story = {
  args: { providers: [openrouterUnexpected], expand: "openrouter" },
};

/** Set up and never tested: the card offers the test instead of a verdict. */
export const NotTested: Story = {
  args: { providers: [xaiUntested], expand: "xai" },
};

/** Ollama on this computer, reached with no key. */
export const LocalConnected: Story = {
  args: { providers: [ollamaConnected], expand: "ollama" },
};

/**
 * A key saved for a server on this computer over plain HTTP. The person
 * agreed to send it in clear text to this loopback address, and says so.
 */
export const KeyOverLoopbackHttp: Story = {
  args: { providers: [compatibleWithKey], expand: "openai_compatible" },
};

/** The badge list at the 420 px compact pane. */
export const BadgesCompact: Story = {
  args: Badges.args,
  globals: { viewport: { value: "compact", isRotated: false } },
};

/** A tested card at the 720 × 480 minimum window. */
export const KeyRejectedMinimumWindow: Story = {
  args: KeyRejected.args,
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};
