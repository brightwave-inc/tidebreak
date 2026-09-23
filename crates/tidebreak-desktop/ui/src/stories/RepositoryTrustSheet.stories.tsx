import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import type { CodeProjectConfigFile } from "@/api/types";
import { RepositoryTrustSheet } from "@/code/RepositoryTrustSheet";

/** The sheet portals to `document.body`, so stories query the screen. */
const body = (canvasElement: HTMLElement) =>
  within(canvasElement.ownerDocument.body);

const claudeHooksAndServers: CodeProjectConfigFile[] = [
  {
    path: ".claude/settings.json",
    engines: ["claude_code"],
    effects: [
      { kind: "hooks", count: 2 },
      { kind: "environment_variables", count: 1 },
      { kind: "permission_rules", count: 3 },
    ],
  },
  {
    path: ".mcp.json",
    engines: ["claude_code"],
    effects: [{ kind: "mcp_servers", count: 1 }],
  },
  {
    path: "CLAUDE.md",
    engines: ["claude_code", "opencode"],
    effects: [{ kind: "instructions", count: 1 }],
  },
];

const everyEngine: CodeProjectConfigFile[] = [
  ...claudeHooksAndServers,
  {
    path: ".claude/skills/",
    engines: ["claude_code", "opencode"],
    effects: [{ kind: "skills", count: 4 }],
  },
  {
    path: ".codex/config.toml",
    engines: ["codex"],
    effects: [
      { kind: "mcp_servers", count: 2 },
      { kind: "environment_variables", count: 2 },
      { kind: "settings", count: 1 },
    ],
  },
  {
    path: "AGENTS.md",
    engines: ["codex", "opencode"],
    effects: [{ kind: "instructions", count: 1 }],
  },
  {
    path: "opencode.jsonc",
    engines: ["opencode"],
    effects: [
      { kind: "plugins", count: 1 },
      { kind: "helper_commands", count: 2 },
    ],
  },
  {
    path: ".opencode/plugin/",
    engines: ["opencode"],
    effects: [{ kind: "plugins", count: 3 }],
  },
  {
    path: ".opencode/package.json",
    engines: ["opencode"],
    effects: [{ kind: "packages", count: 6 }],
  },
];

/**
 * Asked before the first session in a repository whose checkout has its own
 * engine settings. A dialog, not an approval card: the answer is recorded
 * for the repository, and "Continue without its settings" is the safe
 * default that takes focus first.
 */
const meta = {
  title: "Code/Repository trust sheet",
  component: RepositoryTrustSheet,
  parameters: { layout: "fullscreen" },
  args: {
    open: true,
    repoLabel: "brightwave-inc/tidebreak",
    files: claudeHooksAndServers,
    onChoose: fn(),
    onDismiss: fn(),
  },
} satisfies Meta<typeof RepositoryTrustSheet>;
export default meta;
type Story = StoryObj<typeof meta>;

/** A repository with Claude Code hooks, an MCP server, and instructions. */
export const HooksAndServers: Story = {
  play: async ({ canvasElement, args }) => {
    const screen = body(canvasElement);
    await screen.findByText(".claude/settings.json");
    await userEvent.click(
      screen.getByRole("button", { name: "Trust this repository" }),
    );
    await expect(args.onChoose).toHaveBeenCalledWith("trust");
  },
};

/** Settings for every engine at once: the list scrolls inside the sheet. */
export const EveryEngine: Story = {
  args: { files: everyEngine },
};

/**
 * A file Tidebreak could not summarize still loads, so the sheet lists it
 * rather than leaving it out.
 */
export const UnreadSettings: Story = {
  args: {
    files: [
      {
        path: ".claude/settings.json",
        engines: ["claude_code"],
        effects: [],
      },
    ],
  },
};

/** No repository label on the workspace: the copy names no repository. */
export const WithoutRepositoryName: Story = {
  args: { repoLabel: null },
};

/** Trust is being recorded; both answers wait for it. */
export const Saving: Story = {
  args: { saving: "trust" },
};

/** The decision could not be recorded; the sheet stays open to try again. */
export const SaveFailed: Story = {
  args: {
    error:
      "Could not record the trust decision in the repository's git config: could not lock config file .git/config: File exists",
  },
};

/** A phone-width window: the answers stack and long paths wrap. */
export const Narrow: Story = {
  args: {
    files: [
      ...everyEngine,
      {
        path: ".claude/settings.local.json",
        engines: ["claude_code"],
        effects: [{ kind: "permission_rules", count: 12 }],
      },
    ],
  },
  globals: { viewport: { value: "mobile1", isRotated: false } },
};
