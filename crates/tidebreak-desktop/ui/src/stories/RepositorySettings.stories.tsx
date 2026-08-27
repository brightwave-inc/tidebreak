import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import type { CodeRepoSnapshot } from "@/api/types";
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

function client(repo: CodeRepoSnapshot | null, delayMs = 0) {
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
  };
}

/**
 * The only surface that writes a repo's lifecycle hooks: the base a workspace
 * branches from, its branch prefix, the setup and archive scripts, and the
 * named commands a workspace can run. Fields commit on blur; switches commit
 * on change.
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
