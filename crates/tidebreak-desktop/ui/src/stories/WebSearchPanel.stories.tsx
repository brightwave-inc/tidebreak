import type { Meta, StoryObj } from "@storybook/react-vite";

import type { ApiClient, WebSearchConfigInfo } from "@/api";
import { WebSearchPanel } from "@/settings/WebSearchPanel";
import {
  webSearchCredentials,
  webSearchFirecrawlCredentials,
  webSearchFirecrawlReady,
  webSearchHostOnlyUnconfigured,
  webSearchKeyMissing,
  webSearchNoProvider,
  webSearchOff,
  webSearchReady,
  webSearchVendorOnly,
} from "./fixtures";

/**
 * A client that answers the panel's two reads and refuses to write.
 *
 * The panel resolves its whole verdict from what these return, so a fixture per
 * config is enough to render every state. Writes are absent on purpose: a story
 * that could mutate settings would be a story that behaves differently the
 * second time it is opened.
 */
function stubClient(
  config: WebSearchConfigInfo,
  credentials = webSearchCredentials,
): ApiClient {
  return {
    getWebSearchConfig: async () => config,
    listWebSearchCredentials: async () => ({ credentials }),
  } as unknown as ApiClient;
}

/**
 * Web-search settings. The states that matter here are the verdicts, because
 * the verdict is the only place the panel says who will actually run a search —
 * an engine keyed on this host, or the model the chat is already talking to.
 */
const meta = {
  title: "Settings/Web search",
  component: WebSearchPanel,
  args: { client: stubClient(webSearchReady) },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof WebSearchPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The operator's own engine, keyed and preferred over any built-in search. */
export const ConfiguredEngine: Story = {};

/** Firecrawl selected with its write-only key stored in the system keychain. */
export const FirecrawlConfigured: Story = {
  args: {
    client: stubClient(webSearchFirecrawlReady, webSearchFirecrawlCredentials),
  },
};

/**
 * Nothing configured, automatic mode — the common install.
 *
 * This is the state behind the original bug: it used to report "built-in search
 * only" as a *problem*, because a GPT or Gemini chat had no search at all. Now
 * it is a working configuration and reads as one.
 */
export const NoProviderConfigured: Story = {
  args: { client: stubClient(webSearchNoProvider) },
};

/**
 * An engine selected but never keyed. Automatic does not strand the reader
 * here: it says what is missing and what happens until it is supplied.
 */
export const SelectedEngineMissingItsKey: Story = {
  args: { client: stubClient(webSearchKeyMissing) },
};

/** Deliberately no host engine: every search goes to the chat's own model. */
export const BuiltInSearchOnly: Story = {
  args: { client: stubClient(webSearchVendorOnly) },
};

/**
 * The one state that can still strand a chat. Explicit host mode never falls
 * back to the model provider, so an unkeyed engine here means no search — which
 * is the operator's choice, and is reported as unconfigured rather than ready.
 */
export const HostOnlyWithoutAKey: Story = {
  args: { client: stubClient(webSearchHostOnlyUnconfigured) },
};

/** Search turned off outright: the tool is never offered. */
export const Off: Story = {
  args: { client: stubClient(webSearchOff) },
};
