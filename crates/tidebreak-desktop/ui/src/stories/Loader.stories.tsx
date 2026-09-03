import type { ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";

import { Loader } from "@/components/motion/loader";
import { AttentionBadge } from "@/code/AttentionBadge";
import { HARNESS_ICONS } from "@/code/HarnessPicker";
import { SessionLifecycleIndicator } from "@/code/SessionLifecycleIndicator";
import { WorkspaceStatusMark } from "@/code/WorkspaceCard";
import { ToolStatusIcon } from "@/ToolStatusIcon";

import { attentionWorking } from "./fixtures";

const ClaudeIcon = HARNESS_ICONS.claude_code;

const meta = {
  title: "Foundations/Loader",
  component: Loader,
  args: {
    variant: "comet",
    className: "text-live",
  },
} satisfies Meta<typeof Loader>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Comet: Story = {
  args: {
    size: 24,
    label: "Working",
  },
};

export const CometSizes: Story = {
  render: () => (
    <div className="flex items-center gap-6 text-live">
      {[12, 14, 16, 24, 32].map((size) => (
        <div key={size} className="flex items-center gap-2">
          <Loader variant="comet" size={size} label={`${size}px comet`} />
          <span className="text-xs text-muted-foreground">{size}px</span>
        </div>
      ))}
    </div>
  ),
};

export const StatusContexts: Story = {
  render: () => (
    <div className="grid w-80 gap-3 rounded-xl border border-border-subtle bg-background p-4">
      <StatusRow label="Workspace running">
        <WorkspaceStatusMark rank="running" />
      </StatusRow>
      <StatusRow label="Agent working">
        <SessionLifecycleIndicator
          lifecycle="running"
          harness="codex"
          version="0.84.0"
          unrecognizedEventCount={0}
          runningLabel="Agent working"
        />
      </StatusRow>
      <StatusRow label="Compact agent tab">
        <span className="flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-xs font-medium shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_7%,transparent),inset_0_0_0_1px_var(--border-subtle)]">
          <ClaudeIcon className="size-3.5 shrink-0" />
          <span>Main agent</span>
          <AttentionBadge attention={attentionWorking} compact />
        </span>
      </StatusRow>
      <StatusRow label="Tool running">
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <ToolStatusIcon tone="running" className="size-3.5" />
          Running command
        </span>
      </StatusRow>
    </div>
  ),
};

function StatusRow({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex min-h-8 items-center justify-between gap-4 border-t border-border-subtle pt-3 first:border-t-0 first:pt-0">
      <span className="text-xs text-muted-foreground">{label}</span>
      {children}
    </div>
  );
}
