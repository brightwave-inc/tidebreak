import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import { Archive, GitPullRequest, Plus, Terminal } from "lucide-react";

import { CommandPaletteList } from "@/CommandPaletteList";
import { rankPaletteRows, type PaletteRow } from "@/CommandPalette";

/**
 * The Cmd+K palette's list.
 *
 * Rows are fixtures here rather than store reads, and `command` is fixed per
 * story, so what the story draws is the same wherever it is opened. The
 * ranking is the real one: each story passes its rows through
 * `rankPaletteRows`, so section order, the per-section cap, and the fuzzy
 * match are what ship rather than a hand-arranged list.
 */

const workspaceRows: PaletteRow[] = [
  {
    id: "workspace:ws-2",
    section: "workspaces",
    label: "Pluggable memory system",
    hint: "tidebreak · 5m",
    tone: "running",
    onSelect: fn(),
  },
  {
    id: "workspace:ws-3",
    section: "workspaces",
    label: "feat(web): step the New app dialog instead of one long scroll",
    hint: "tidebreak · 12m",
    tone: "warning",
    onSelect: fn(),
  },
  {
    id: "workspace:ws-4",
    section: "workspaces",
    label: "feat(code): draft the create-PR request into the composer",
    hint: "tidebreak · 14m",
    tone: "ready",
    onSelect: fn(),
  },
  {
    id: "workspace:ws-5",
    section: "workspaces",
    label: "Sentry catalog gaps",
    hint: "gateway · 2h",
    tone: "neutral",
    onSelect: fn(),
  },
  {
    id: "workspace:ws-6",
    section: "workspaces",
    label: "Pr state sync lag",
    hint: "tidebreak · 1d",
    tone: "critical",
    onSelect: fn(),
  },
];

const actionRows: PaletteRow[] = [
  {
    id: "action:new-session",
    section: "actions",
    label: "New session",
    icon: Plus,
    onSelect: fn(),
  },
  {
    id: "action:toggle-terminal",
    section: "actions",
    label: "Toggle terminal",
    icon: Terminal,
    shortcut: "toggle-code-terminal",
    onSelect: fn(),
  },
  {
    id: "action:rename",
    section: "actions",
    label: "Rename",
    onSelect: fn(),
  },
  {
    id: "action:archive",
    section: "actions",
    label: "Archive",
    icon: Archive,
    shortcut: "code-archive-workspace",
    onSelect: fn(),
  },
];

const shipRows: PaletteRow[] = [
  {
    id: "ship:pull_request",
    section: "ship",
    label: "Push and open a pull request",
    icon: GitPullRequest,
    shortcut: "code-create-pr",
    onSelect: fn(),
  },
  {
    id: "ship:merge",
    section: "ship",
    label: "Merge, or auto-merge once the checks pass",
    icon: GitPullRequest,
    shortcut: "code-merge-pr",
    onSelect: fn(),
  },
  {
    id: "ship:watch",
    section: "ship",
    label: "Watch the pull request and fix failures",
    icon: GitPullRequest,
    shortcut: "code-watch-pr",
    onSelect: fn(),
  },
];

const settingsRows: PaletteRow[] = [
  {
    id: "settings:models",
    section: "settings",
    label: "Models",
    onSelect: fn(),
  },
  {
    id: "settings:appearance",
    section: "settings",
    label: "Appearance",
    onSelect: fn(),
  },
  {
    id: "settings:coding-harnesses",
    section: "settings",
    label: "Coding harnesses",
    onSelect: fn(),
  },
];

const navigateRows: PaletteRow[] = [
  {
    id: "navigate:pull-requests",
    section: "navigate",
    label: "Pull requests",
    onSelect: fn(),
  },
  { id: "navigate:runs", section: "navigate", label: "Runs", onSelect: fn() },
  {
    id: "navigate:chat",
    section: "navigate",
    label: "Go to chat",
    onSelect: fn(),
  },
];

const suggestedRow: PaletteRow = {
  id: "suggested:create_pr",
  section: "suggested",
  label: "Open pull request",
  hint: "2 commits ahead of main",
  tone: "ready",
  shortcut: "code-workflow-next",
  onSelect: fn(),
};

const codeRows: PaletteRow[] = [
  suggestedRow,
  ...workspaceRows,
  ...actionRows,
  ...shipRows,
  ...navigateRows,
  ...settingsRows,
];

const chatRows: PaletteRow[] = [
  {
    id: "chat:c-1",
    section: "chats",
    label: "Pricing page copy",
    hint: "Marketing",
    onSelect: fn(),
  },
  {
    id: "chat:c-2",
    section: "chats",
    label: "Postgres index review",
    onSelect: fn(),
  },
  {
    id: "navigate:new-chat",
    section: "actions",
    label: "Start new work",
    icon: Plus,
    shortcut: "new-chat",
    onSelect: fn(),
  },
  { id: "navigate:inbox", section: "navigate", label: "Inbox", onSelect: fn() },
  {
    id: "navigate:code",
    section: "navigate",
    label: "Go to code",
    onSelect: fn(),
  },
  ...settingsRows,
];

const meta = {
  component: CommandPaletteList,
  title: "Foundations/Command palette",
  args: {
    query: "",
    onQueryChange: fn(),
    onSelect: fn(),
    command: true,
    mode: "code",
    groups: rankPaletteRows(codeRows, ""),
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl overflow-hidden rounded-xl border border-border-subtle bg-popover shadow-2xl">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof CommandPaletteList>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * Code mode at rest. The workspace's next ship step leads, because that is what
 * a reader mid-flow reaches the palette for; the rail's workspaces follow.
 */
export const CodeMode: Story = {};

/**
 * Typing narrows every section at once, which is the point of not making the
 * reader pick a kind first.
 */
export const Filtered: Story = {
  args: {
    query: "pr",
    groups: rankPaletteRows(codeRows, "pr"),
  },
};

/** `>` drops everything that is not a command, and says so in the chip. */
export const CommandScope: Story = {
  args: {
    query: ">merge",
    scopeLabel: "commands",
    groups: rankPaletteRows(codeRows, ">merge"),
  },
};

/** `@` scopes to workspaces, and lifts the five-row cap now that it is the only section. */
export const WorkspaceScope: Story = {
  args: {
    query: "@",
    scopeLabel: "go to",
    groups: rankPaletteRows(codeRows, "@"),
  },
};

/** Quick access to settings: every section is its own row. */
export const Settings: Story = {
  args: {
    query: "settings",
    groups: rankPaletteRows(
      [
        ...codeRows,
        ...[
          "Providers",
          "Model Gateway",
          "Context",
          "Agents",
          "Permissions",
        ].map<PaletteRow>((label) => ({
          id: `settings:${label}`,
          section: "settings",
          label,
          keywords: "settings",
          onSelect: fn(),
        })),
      ],
      "settings",
    ),
  },
};

/** `#` reads the worktree, which is the one thing here that takes a moment. */
export const FilesLoading: Story = {
  args: {
    query: "#",
    scopeLabel: "files",
    loading: true,
    groups: [],
    emptyLabel: "Reading the worktree…",
  },
};

/** Nothing matched, named so the reader can see what they typed. */
export const NoMatches: Story = {
  args: {
    query: "zzzz",
    groups: rankPaletteRows(codeRows, "zzzz"),
    emptyLabel: "Nothing matches “zzzz”.",
  },
};

/** Chat mode, where conversations are the things and there is nothing to ship. */
export const ChatMode: Story = {
  args: {
    mode: "chat",
    groups: rankPaletteRows(chatRows, ""),
  },
};

/** The same list on a keyboard whose modifier is Ctrl rather than Cmd. */
export const WindowsKeycaps: Story = {
  args: { command: false },
};
