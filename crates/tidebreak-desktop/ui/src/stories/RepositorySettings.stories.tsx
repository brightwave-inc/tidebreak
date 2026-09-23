import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import type { CodeRepoSnapshot, CodeRepoTrustSnapshot } from "@/api/types";
import { RepositorySettings } from "@/code/RepositorySettings";
import { codeRepositories } from "./fixtures";

const stored: CodeRepoSnapshot = {
  ...codeRepositories[0],
  setup_script: "pnpm install --frozen-lockfile",
  archive_script: "./scripts/back-up-worktree.sh",
  quick_actions: [
    {
      name: "Test",
      command: "cargo test -p tidebreak-server",
      auto_run_on_create: false,
    },
    { name: "Install", command: "pnpm install", auto_run_on_create: true },
  ],
};

/** An undecided repository whose checkout has Claude Code settings. */
const undecided: CodeRepoTrustSnapshot = {
  repo_id: stored.id,
  trust: "undecided",
  files: [
    {
      path: ".claude/settings.json",
      engines: ["claude_code"],
      effects: [
        { kind: "hooks", count: 2 },
        { kind: "environment_variables", count: 1 },
      ],
    },
    {
      path: ".mcp.json",
      engines: ["claude_code"],
      effects: [{ kind: "mcp_servers", count: 1 }],
    },
    {
      path: "AGENTS.md",
      engines: ["codex", "opencode"],
      effects: [{ kind: "instructions", count: 1 }],
    },
  ],
};

function client(
  repo: CodeRepoSnapshot | null,
  delayMs = 0,
  trust: CodeRepoTrustSnapshot = undecided,
) {
  return {
    getCodeRepo: async () => {
      if (delayMs) await new Promise((done) => setTimeout(done, delayMs));
      if (!repo) throw new Error("repo 404");
      return repo;
    },
    patchCodeRepo: async (
      _id: string,
      body: Partial<CodeRepoSnapshot>,
    ): Promise<CodeRepoSnapshot> => ({ ...(repo ?? stored), ...body }),
    getCodeRepoTrust: async () => {
      if (delayMs) await new Promise((done) => setTimeout(done, delayMs));
      if (!repo) throw new Error("repo 404");
      return trust;
    },
    setCodeRepoTrust: async (
      _id: string,
      trusted: boolean,
    ): Promise<CodeRepoTrustSnapshot> => ({
      ...trust,
      trust: trusted ? "trusted" : "untrusted",
    }),
  };
}

/**
 * The only surface that writes a repo's lifecycle hooks: the base a workspace
 * branches from, its branch prefix, the setup and archive scripts, and the
 * named commands a workspace can run. Fields use SettingsField and commit on
 * blur; switches commit on change.
 */
const meta = {
  title: "Code/Repository settings",
  component: RepositorySettings,
  args: {
    client: client(stored) as never,
    repoId: "repo-tidebreak",
    repoLabel: "brightwave-inc/tidebreak",
    onSaved: fn(),
  },
  decorators: [
    (Story) => (
      <div className="max-w-2xl px-5 py-6">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof RepositorySettings>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Both scripts set and two quick actions, one of them auto-run on create. */
export const Configured: Story = {};

/**
 * A trusted repository: the switch is on, and the checkout's engine settings
 * are listed so the reader sees what trust covers. Turning it off revokes.
 */
export const Trusted: Story = {
  args: {
    client: client(stored, 0, { ...undecided, trust: "trusted" }) as never,
  },
};

/**
 * The reader chose to continue without the repository's settings; engines
 * skip them until the switch goes on.
 */
export const NotTrusted: Story = {
  args: {
    client: client(stored, 0, { ...undecided, trust: "untrusted" }) as never,
  },
};

/** A checkout with no engine settings of its own. */
export const NoEngineSettings: Story = {
  args: {
    client: client(stored, 0, { ...undecided, files: [] }) as never,
  },
};

/** A fresh registration: defaults for the refs, no scripts, no actions. */
export const Empty: Story = {
  args: {
    client: client({
      ...stored,
      setup_script: undefined,
      archive_script: undefined,
      quick_actions: [],
    }) as never,
  },
};

/** The read is still in flight; the spinner is the only chrome that moves. */
export const Loading: Story = {
  args: { client: client(stored, 100_000) as never },
};

/** The repository is tracked on GitHub but never registered in Tidebreak. */
export const NotRegistered: Story = {
  args: { repoId: null },
};

/** The read failed. The error names a retry rather than an empty form. */
export const LoadFailed: Story = {
  args: { client: client(null) as never },
};

/**
 * The tracked-repos dialog is a dense column. Refs stack, quick-action rows
 * wrap, and the error retry stays reachable instead of clipping the copy.
 */
export const Narrow: Story = {
  decorators: [
    (Story) => (
      <div className="w-[320px] px-3 py-4">
        <Story />
      </div>
    ),
  ],
};
