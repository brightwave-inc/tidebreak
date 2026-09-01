import { useState } from "react";
import { DndContext } from "@dnd-kit/core";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn, userEvent, within } from "storybook/test";

import {
  CodeCenterTabs,
  type CodeConversationTab,
} from "@/code/CodeCenterTabs";
import type { PanelContent } from "@/panel/panelTypes";

import {
  attentionDoneUnreviewed,
  attentionNeedsYou,
  attentionWorking,
} from "./fixtures";

const MAIN_AGENT: CodeConversationTab[] = [
  { id: "session-1", label: "Main agent", harness: "claude_code" },
];

/**
 * The center tab strip: one tab per agent in the workspace, then editor tabs,
 * then the single `+` that offers everything the center can open. That menu
 * has two groups — the three things a workspace can hold many of (agent,
 * terminal, browser), then the views onto the worktree.
 */
function TabStrip({
  editorTabs,
  conversations = MAIN_AGENT,
  withBrowser = true,
  withDiff = true,
  withTerminal = true,
  canNewTerminal = true,
  region = "primary" as const,
}: {
  editorTabs: PanelContent[];
  conversations?: CodeConversationTab[];
  withBrowser?: boolean;
  withDiff?: boolean;
  withTerminal?: boolean;
  canNewTerminal?: boolean;
  region?: "primary" | "secondary";
}) {
  const [active, setActive] = useState(editorTabs.length > 0 ? 0 : -1);
  const [chatFocused, setChatFocused] = useState(editorTabs.length === 0);
  const [conversation, setConversation] = useState<string | null>(
    conversations[0]?.id ?? null,
  );
  // Tabs drag through dnd-kit, which addresses everything from the context the
  // page provides. Without one here the strip would render but sit inert.
  return (
    <DndContext>
      <CodeCenterTabs
        editorTabs={editorTabs}
        editorActiveIndex={active}
        conversationFocused={chatFocused}
        conversations={region === "primary" ? conversations : []}
        activeConversationId={conversation}
        onSelectConversation={(id) => {
          setChatFocused(true);
          setConversation(id);
        }}
        onNewConversation={region === "primary" ? fn() : undefined}
        onCloseConversation={fn()}
        onForkConversation={fn()}
        onSelectEditor={(index) => {
          setChatFocused(false);
          setActive(index);
        }}
        onCloseEditor={fn()}
        onCloseAllEditors={fn()}
        onCloseOtherEditors={fn()}
        onCloseEditorsToRight={fn()}
        onCopyPath={fn()}
        onNewTab={fn()}
        onNewBrowser={withBrowser ? fn() : undefined}
        onNewDiff={withDiff ? fn() : undefined}
        onNewSourceControl={withDiff ? fn() : undefined}
        onNewPr={withDiff ? fn() : undefined}
        onNewTerminal={withTerminal ? fn() : undefined}
        canNewTerminal={canNewTerminal}
        onSplitActive={fn()}
        region={region}
        browserTitles={{ "browser-1": "Storybook — Tidebreak" }}
        terminalLabels={{
          "term-1": "Terminal 1",
          "term-2": "Terminal 2",
          "term-3": "Terminal 3",
        }}
      />
    </DndContext>
  );
}

const meta = {
  title: "Code/Center tabs",
  component: TabStrip,
  args: { editorTabs: [] },
  decorators: [
    (Story) => (
      <div className="border-border-subtle mx-auto max-w-3xl rounded-md border pt-0">
        <Story />
        <div className="text-foreground-subtle p-8 text-center text-xs">
          tab panel
        </div>
      </div>
    ),
  ],
} satisfies Meta<typeof TabStrip>;

export default meta;
type Story = StoryObj<typeof meta>;

/** One agent alone; the `+` menu is the whole affordance. */
export const ConversationOnly: Story = {};

/** The live mark on the agent the reader is looking at. */
export const WorkingConversation: Story = {
  args: {
    conversations: [
      {
        id: "session-1",
        label: "Main agent",
        harness: "claude_code",
        attention: attentionWorking,
      },
    ],
  },
};

/**
 * The `+` menu, open. The top group starts something the workspace can hold
 * many of; the group under it opens the one view there is of the worktree.
 */
export const NewTabMenu: Story = {
  play: async ({ canvasElement }) => {
    await userEvent.click(
      within(canvasElement).getByRole("button", { name: "New tab" }),
    );
  },
};

/**
 * Several agents in one worktree, each showing its own state: one waiting on a
 * reply, one still working, one idle. The dots are the agents' states, so they
 * stay readable on the tabs nobody is looking at.
 */
export const ManyConversations: Story = {
  args: {
    conversations: [
      {
        id: "session-1",
        label: "Main agent",
        harness: "claude_code",
        attention: attentionNeedsYou,
      },
      {
        id: "session-2",
        label: "Codex",
        harness: "codex",
        attention: attentionWorking,
        closable: true,
      },
      {
        id: "session-3",
        label: "opencode",
        harness: "opencode",
        attention: attentionDoneUnreviewed,
        closable: true,
      },
    ],
  },
};

/**
 * Right-click an agent's tab to fork it: the transcript goes into the
 * worktree and a new agent opens on it. A draft has nothing to fork yet.
 */
export const ForkFromTabMenu: Story = {
  args: { conversations: MAIN_AGENT },
};

/** A new agent the reader has opened but not started. */
export const DraftConversation: Story = {
  args: {
    conversations: [
      ...MAIN_AGENT,
      { id: null, label: "New agent", closable: true },
    ],
  },
};

export const FilesOpen: Story = {
  args: {
    editorTabs: [
      { type: "file", path: "crates/tidebreak-server/src/code/watch.rs" },
      { type: "file", path: "docs/code-mode.md" },
      { type: "diff" },
    ],
  },
};

/** Source control and the PR's details are peer tabs, not sidebar-only. */
export const WorkflowTabs: Story = {
  args: {
    editorTabs: [
      { type: "source_control" },
      { type: "pr" },
      { type: "file", path: "src/lib.rs" },
    ],
  },
};

export const WithBrowserTab: Story = {
  args: {
    editorTabs: [
      { type: "file", path: "src/lib.rs" },
      { type: "browser", browserId: "browser-1" },
    ],
  },
};

/**
 * Several shells at once. Each is its own tab over its own process, numbered
 * so they stay tellable apart, and each drags and reorders like a file.
 */
export const ManyTerminals: Story = {
  args: {
    editorTabs: [
      { type: "terminal", terminalId: "term-1" },
      { type: "terminal", terminalId: "term-2" },
      { type: "file", path: "src/lib.rs" },
      { type: "terminal", terminalId: "term-3" },
    ],
  },
};

/** At the workspace's shell limit, the menu stops offering another. */
export const TerminalLimitReached: Story = {
  args: {
    editorTabs: [{ type: "terminal", terminalId: "term-1" }],
    canNewTerminal: false,
  },
};

/** A split's right-hand group: no agent tabs, close-group affordance. */
export const SecondaryGroup: Story = {
  args: {
    region: "secondary",
    editorTabs: [{ type: "diff", path: "src/lib.rs" }],
  },
};

/** Only the file picker is offered when a region has no other openers. */
export const MinimalMenu: Story = {
  args: {
    editorTabs: [],
    withBrowser: false,
    withDiff: false,
    withTerminal: false,
  },
};
