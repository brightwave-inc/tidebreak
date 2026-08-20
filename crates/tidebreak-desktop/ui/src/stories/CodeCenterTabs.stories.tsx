import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { CodeCenterTabs } from "@/code/CodeCenterTabs";
import type { PanelContent } from "@/panel/panelTypes";

/**
 * The center tab strip: the persistent Main agent tab, editor tabs, and the
 * single `+` that offers everything the center can open — a file, the
 * all-changes diff, a browser, or the terminal — instead of jumping straight
 * into the file picker.
 */
function TabStrip({
  editorTabs,
  withBrowser = true,
  withDiff = true,
  withTerminal = true,
  region = "primary" as const,
}: {
  editorTabs: PanelContent[];
  withBrowser?: boolean;
  withDiff?: boolean;
  withTerminal?: boolean;
  region?: "primary" | "secondary";
}) {
  const [active, setActive] = useState(editorTabs.length > 0 ? 0 : -1);
  const [chatFocused, setChatFocused] = useState(editorTabs.length === 0);
  return (
    <CodeCenterTabs
      editorTabs={editorTabs}
      editorActiveIndex={active}
      conversationFocused={chatFocused}
      onSelectChat={() => setChatFocused(true)}
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
      onSplitActive={fn()}
      region={region}
      showMainAgent={region === "primary"}
      browserTitles={{ "browser-1": "Storybook — Tidebreak" }}
    />
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

/** Conversation alone; the `+` menu is the whole affordance. */
export const ConversationOnly: Story = {};

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

/** A split's right-hand group: no Main agent tab, close-group affordance. */
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
