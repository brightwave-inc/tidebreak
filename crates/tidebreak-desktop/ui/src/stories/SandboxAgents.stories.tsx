import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import {
  SandboxAgentsSection,
  SandboxAgentDetail,
  type SandboxAgent,
  type SandboxHarness,
  type SandboxStatus,
} from "@/sidebar/SandboxAgentsSection";
import {
  SpawnSandboxDialog,
  type SandboxProfile,
} from "@/sidebar/SpawnSandboxDialog";
import { TooltipProvider } from "@/components/ui/tooltip";

// ---------------------------------------------------------------------------
// Fixture data
// ---------------------------------------------------------------------------

function sandboxAgent(
  id: string,
  harness: SandboxHarness,
  status: SandboxStatus,
  task: string,
  overrides: Partial<SandboxAgent> = {},
): SandboxAgent {
  const live = ["queued", "provisioning", "running", "idle"].includes(status);
  return {
    id,
    harness,
    task,
    status,
    profile: "default",
    elapsedLabel: live ? "2m 34s" : "8m 12s",
    spendMicroUsd: live ? 45_000 : 320_000,
    ...overrides,
  };
}

const liveAgents: SandboxAgent[] = [
  sandboxAgent(
    "sb-1",
    "claude_code",
    "running",
    "Refactor the settings panel to use the new token system",
    {
      repositoryUrl: "https://github.com/brightwave-inc/tidebreak",
      repositoryRef: "feat/settings-tokens",
      elapsedLabel: "4m 12s",
      spendMicroUsd: 89_000,
    },
  ),
  sandboxAgent(
    "sb-2",
    "codex",
    "running",
    "Add integration tests for the webhook handler",
    {
      repositoryUrl: "https://github.com/brightwave-inc/model-gateway",
      repositoryRef: "main",
      elapsedLabel: "1m 48s",
      spendMicroUsd: 32_000,
    },
  ),
  sandboxAgent(
    "sb-3",
    "grok_build",
    "provisioning",
    "Migrate the database schema for user preferences",
    { elapsedLabel: "12s" },
  ),
];

const mixedAgents: SandboxAgent[] = [
  sandboxAgent(
    "sb-1",
    "claude_code",
    "running",
    "Audit the permission model for connected apps",
    {
      repositoryUrl: "https://github.com/brightwave-inc/tidebreak",
      repositoryRef: "main",
      elapsedLabel: "6m 30s",
      spendMicroUsd: 142_000,
    },
  ),
  sandboxAgent(
    "sb-2",
    "opencode",
    "idle",
    "Review accessibility compliance across all forms",
    { elapsedLabel: "3m 15s" },
  ),
  sandboxAgent(
    "sb-3",
    "codex",
    "completed",
    "Fix the rate limiter edge case in batch processing",
    { elapsedLabel: "12m 45s", spendMicroUsd: 520_000 },
  ),
  sandboxAgent(
    "sb-4",
    "grok_build",
    "failed",
    "Set up E2E test infrastructure for mobile builds",
    { elapsedLabel: "2m 10s", spendMicroUsd: 28_000 },
  ),
  sandboxAgent(
    "sb-5",
    "claude_code",
    "cancelled",
    "Investigate memory leak in the WebSocket reconnect loop",
  ),
];

const singleRunning: SandboxAgent[] = [
  sandboxAgent(
    "sb-1",
    "claude_code",
    "running",
    "Ship the sandbox agents sidebar section",
    {
      repositoryUrl: "https://github.com/brightwave-inc/tidebreak",
      repositoryRef: "feat/sandbox-ui",
      elapsedLabel: "2m 05s",
      spendMicroUsd: 55_000,
    },
  ),
];

const profiles: SandboxProfile[] = [
  { id: "default", name: "Default", enabled: true },
  { id: "high-resource", name: "High resource", enabled: true },
  { id: "restricted", name: "Restricted (disabled)", enabled: false },
];

// ---------------------------------------------------------------------------
// Wrapper
// ---------------------------------------------------------------------------

function SidebarWrapper({
  children,
  width = 260,
}: {
  children: React.ReactNode;
  width?: number;
}) {
  return (
    <TooltipProvider>
      <div
        className="flex h-[600px] border-r border-border-subtle bg-page-background"
        style={{ width }}
      >
        <div className="flex w-full flex-col overflow-hidden">
          <div className="mt-2 flex grow flex-col gap-1 overflow-y-auto px-2">
            {children}
          </div>
        </div>
      </div>
    </TooltipProvider>
  );
}

// ---------------------------------------------------------------------------
// Section stories
// ---------------------------------------------------------------------------

const meta = {
  title: "Sidebar/Sandbox agents",
  parameters: { layout: "centered" },
} satisfies Meta;

export default meta;

export const Empty: StoryObj = {
  render: () => (
    <SidebarWrapper>
      <SandboxAgentsSection agents={[]} onSpawn={fn()} />
    </SidebarWrapper>
  ),
};

export const SingleRunning: StoryObj = {
  render: () => (
    <SidebarWrapper>
      <SandboxAgentsSection
        agents={singleRunning}
        onSpawn={fn()}
        onOpen={fn()}
        onStop={fn()}
      />
    </SidebarWrapper>
  ),
};

export const ThreeLive: StoryObj = {
  name: "3 live agents",
  render: () => (
    <SidebarWrapper>
      <SandboxAgentsSection
        agents={liveAgents}
        onSpawn={fn()}
        onOpen={fn()}
        onStop={fn()}
      />
    </SidebarWrapper>
  ),
};

export const MixedStatuses: StoryObj = {
  name: "Mixed statuses (5 agents)",
  render: () => (
    <SidebarWrapper>
      <SandboxAgentsSection
        agents={mixedAgents}
        onSpawn={fn()}
        onOpen={fn()}
        onStop={fn()}
      />
    </SidebarWrapper>
  ),
};

export const NoSpawnButton: StoryObj = {
  name: "Read-only (no spawn)",
  render: () => (
    <SidebarWrapper>
      <SandboxAgentsSection agents={mixedAgents.slice(0, 2)} onOpen={fn()} />
    </SidebarWrapper>
  ),
};

export const NarrowRail: StoryObj = {
  name: "Narrow rail (200px)",
  render: () => (
    <SidebarWrapper width={200}>
      <SandboxAgentsSection
        agents={liveAgents}
        onSpawn={fn()}
        onOpen={fn()}
        onStop={fn()}
      />
    </SidebarWrapper>
  ),
};

// ---------------------------------------------------------------------------
// Detail stories
// ---------------------------------------------------------------------------

export const DetailRunning: StoryObj = {
  name: "Agent detail — running",
  render: () => (
    <div className="w-80 rounded-lg border border-border bg-background">
      <SandboxAgentDetail agent={liveAgents[0]!} />
    </div>
  ),
};

export const DetailCompleted: StoryObj = {
  name: "Agent detail — completed",
  render: () => (
    <div className="w-80 rounded-lg border border-border bg-background">
      <SandboxAgentDetail agent={mixedAgents[2]!} />
    </div>
  ),
};

export const DetailFailed: StoryObj = {
  name: "Agent detail — failed",
  render: () => (
    <div className="w-80 rounded-lg border border-border bg-background">
      <SandboxAgentDetail agent={mixedAgents[3]!} />
    </div>
  ),
};

// ---------------------------------------------------------------------------
// Spawn dialog stories
// ---------------------------------------------------------------------------

export const SpawnDialog: StoryObj = {
  name: "Spawn dialog",
  render: () => (
    <SpawnSandboxDialog
      open
      profiles={profiles}
      onSubmit={fn()}
      onClose={fn()}
    />
  ),
};

export const SpawnDialogNoProfiles: StoryObj = {
  name: "Spawn dialog — no profiles",
  render: () => (
    <SpawnSandboxDialog open profiles={[]} onSubmit={fn()} onClose={fn()} />
  ),
};

// ---------------------------------------------------------------------------
// All harnesses
// ---------------------------------------------------------------------------

export const AllHarnesses: StoryObj = {
  name: "Every harness type",
  render: () => {
    const agents: SandboxAgent[] = [
      sandboxAgent(
        "h-1",
        "claude_code",
        "running",
        "Claude Code agent working on auth",
      ),
      sandboxAgent("h-2", "codex", "running", "Codex agent building API tests"),
      sandboxAgent(
        "h-3",
        "opencode",
        "running",
        "opencode agent reviewing forms",
      ),
      sandboxAgent(
        "h-4",
        "grok_build",
        "running",
        "Grok agent migrating schemas",
      ),
      sandboxAgent(
        "h-5",
        "custom",
        "running",
        "Custom harness processing data",
      ),
    ];
    return (
      <SidebarWrapper>
        <SandboxAgentsSection
          agents={agents}
          onSpawn={fn()}
          onOpen={fn()}
          onStop={fn()}
        />
      </SidebarWrapper>
    );
  },
};
