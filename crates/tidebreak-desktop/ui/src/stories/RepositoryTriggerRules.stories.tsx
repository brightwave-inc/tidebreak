import type { Meta, StoryObj } from "@storybook/react-vite";

import type { ApiClient } from "@/api/client";
import type {
  CodeGitHubRepositoryRef,
  CodeTriggerAction,
  CodeTriggerCondition,
  CodeTriggerSnapshot,
} from "@/api/types";
import { RepositoryTriggerRules } from "@/code/RepositoryTriggerRules";

const repository: CodeGitHubRepositoryRef = {
  host: "github.com",
  owner: "brightwave-inc",
  name: "tidebreak",
  name_with_owner: "brightwave-inc/tidebreak",
  url: "https://github.com/brightwave-inc/tidebreak",
  tidebreak_repo_id: "repo-storybook",
};

function trigger(
  condition: CodeTriggerCondition,
  action: CodeTriggerAction,
  enabled = true,
): CodeTriggerSnapshot {
  return {
    id: `trigger-${condition}`,
    repo_id: "repo-storybook",
    condition,
    action,
    enabled,
    created_at: "2026-08-29T12:00:00Z",
    updated_at: "2026-08-29T12:00:00Z",
  };
}

type TriggerClient = Pick<
  ApiClient,
  | "listCodeTriggers"
  | "createCodeTrigger"
  | "setCodeTriggerEnabled"
  | "deleteCodeTrigger"
>;

function client(
  triggers: CodeTriggerSnapshot[],
  options: { fail?: boolean; loading?: boolean } = {},
): TriggerClient {
  return {
    listCodeTriggers: async () => {
      if (options.loading) await new Promise(() => undefined);
      if (options.fail)
        throw new Error("Could not reach the Tidebreak server.");
      return triggers;
    },
    createCodeTrigger: async (_repoId, condition, action) =>
      trigger(condition, action),
    setCodeTriggerEnabled: async (_repoId, id, enabled) => ({
      ...(triggers.find((item) => item.id === id) ??
        trigger("checks_failed", "deliver")),
      enabled,
    }),
    deleteCodeTrigger: async () => undefined,
  };
}

const armed = [
  trigger("checks_failed", "deliver"),
  trigger("conflicts", "deliver"),
  trigger("ready_to_merge", "notify"),
];

const meta = {
  title: "Code/Repository triggers",
  component: RepositoryTriggerRules,
  args: {
    client: client(armed),
    repository,
    target: {
      sessionTitle: "Fix the auth flow",
      harnessLabel: "Codex CLI",
      delivery: "steer",
    },
  },
  decorators: [
    (Story) => (
      <div className="max-w-3xl px-5 py-6">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof RepositoryTriggerRules>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The server owns the rules; the repository header names no legacy engine. */
export const Armed: Story = {};

/** A registered repository with no triggers yet. */
export const NothingArmed: Story = {
  args: { client: client([]) },
};

/** The server read is still in flight. */
export const Loading: Story = {
  args: { client: client([], { loading: true }) },
};

/** A failed read keeps its retry action visible. */
export const LoadFailed: Story = {
  args: { client: client([], { fail: true }) },
};

/** The tracked-repositories dialog can narrow without clipping controls. */
export const Narrow: Story = {
  decorators: [
    (Story) => (
      <div className="w-80 px-3 py-4">
        <Story />
      </div>
    ),
  ],
};
