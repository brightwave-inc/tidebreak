import { type ComponentType, useState } from "react";
import { Bot, ChevronRight, GitBranch, Plus, Square } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { ClaudeIcon, OpenAIIcon, OpenCodeIcon, XaiIcon } from "@/ProviderIcons";
import { SidebarButton } from "./primitives";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type SandboxHarness =
  | "claude_code"
  | "codex"
  | "opencode"
  | "grok_build"
  | "custom";

export type SandboxStatus =
  | "queued"
  | "provisioning"
  | "running"
  | "idle"
  | "completed"
  | "failed"
  | "cancelled";

export type SandboxAgent = {
  id: string;
  harness: SandboxHarness;
  task: string;
  status: SandboxStatus;
  repositoryUrl?: string;
  repositoryRef?: string;
  profile: string;
  elapsedLabel?: string;
  spendMicroUsd?: number;
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const HARNESS_ICONS: Record<
  SandboxHarness,
  ComponentType<{ className?: string }>
> = {
  claude_code: ClaudeIcon,
  codex: OpenAIIcon,
  opencode: OpenCodeIcon,
  grok_build: XaiIcon,
  custom: Bot as ComponentType<{ className?: string }>,
};

const HARNESS_LABELS: Record<SandboxHarness, string> = {
  claude_code: "Claude Code",
  codex: "Codex",
  opencode: "opencode",
  grok_build: "Grok",
  custom: "Custom",
};

export const SANDBOX_STATUS_LABELS: Record<SandboxStatus, string> = {
  queued: "Queued",
  provisioning: "Starting",
  running: "Running",
  idle: "Idle",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
};

function sandboxStatusDotClass(status: SandboxStatus): string {
  switch (status) {
    case "queued":
    case "provisioning":
      return "animate-pulse bg-muted-foreground";
    case "running":
      return "animate-pulse bg-live";
    case "idle":
      return "bg-warning";
    case "completed":
      return "bg-success";
    case "failed":
      return "bg-destructive";
    case "cancelled":
      return "bg-muted-foreground";
  }
}

function sandboxStatusBadgeVariant(
  status: SandboxStatus,
): "live" | "info" | "warning" | "success" | "critical" | "outline" {
  switch (status) {
    case "running":
      return "live";
    case "queued":
    case "provisioning":
      return "info";
    case "idle":
      return "warning";
    case "completed":
      return "success";
    case "failed":
      return "critical";
    case "cancelled":
      return "outline";
  }
}

function isLive(status: SandboxStatus): boolean {
  return (
    status === "queued" ||
    status === "provisioning" ||
    status === "running" ||
    status === "idle"
  );
}

function repoShortName(url: string): string {
  const match = url.match(/([^/]+\/[^/]+?)(?:\.git)?$/);
  return match?.[1] ?? url;
}

// ---------------------------------------------------------------------------
// Section
// ---------------------------------------------------------------------------

export function SandboxAgentsSection({
  agents,
  onSpawn,
  onOpen,
  onStop,
}: {
  agents: readonly SandboxAgent[];
  onSpawn?: () => void;
  onOpen?: (id: string) => void;
  onStop?: (id: string) => void;
}) {
  const [collapsed, setCollapsed] = useState(false);
  const liveCount = agents.filter((agent) => isLive(agent.status)).length;

  if (agents.length === 0 && !onSpawn) return null;

  return (
    <div className="flex shrink-0 flex-col">
      <div className="flex items-center">
        <button
          type="button"
          className="flex min-w-0 flex-1 items-center gap-1 px-2 py-1"
          onClick={() => setCollapsed((prev) => !prev)}
          aria-expanded={!collapsed}
        >
          <ChevronRight
            className={cn(
              "size-3 shrink-0 text-muted-foreground transition-transform",
              !collapsed && "rotate-90",
            )}
            aria-hidden="true"
          />
          <span className="text-xs font-medium text-foreground-subtle">
            Agents
          </span>
          {liveCount > 0 && (
            <Badge variant="live" size="sm" className="ml-auto px-1.5 py-0">
              {liveCount}
            </Badge>
          )}
          {liveCount === 0 && agents.length > 0 && (
            <span className="ml-auto text-[10px] text-muted-foreground">
              {agents.length}
            </span>
          )}
        </button>
        {onSpawn && (
          <WithTooltip label="Spawn sandbox agent" side="right">
            <button
              type="button"
              className="mr-1 shrink-0 rounded-md p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              aria-label="Spawn sandbox agent"
              onClick={onSpawn}
            >
              <Plus className="size-3.5" />
            </button>
          </WithTooltip>
        )}
      </div>

      {!collapsed && (
        <div className="flex flex-col gap-0.5">
          {agents.map((agent) => (
            <SandboxAgentRow
              key={agent.id}
              agent={agent}
              onOpen={onOpen}
              onStop={onStop}
            />
          ))}
          {agents.length === 0 && onSpawn && (
            <SidebarButton className="text-muted-foreground" onClick={onSpawn}>
              <Bot className="text-icon-violet" />
              <span className="text-xs">Spawn an agent</span>
            </SidebarButton>
          )}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Row
// ---------------------------------------------------------------------------

function SandboxAgentRow({
  agent,
  onOpen,
  onStop,
}: {
  agent: SandboxAgent;
  onOpen?: (id: string) => void;
  onStop?: (id: string) => void;
}) {
  const Icon = HARNESS_ICONS[agent.harness];
  const live = isLive(agent.status);

  return (
    <div className="group/agent relative">
      <SidebarButton
        className="gap-1.5 pr-1.5"
        onClick={() => onOpen?.(agent.id)}
        aria-label={`${HARNESS_LABELS[agent.harness]} agent: ${agent.task}`}
      >
        <span className="relative flex size-4 shrink-0 items-center justify-center">
          <Icon className="size-3.5" />
          <span
            className={cn(
              "absolute -right-0.5 -bottom-0.5 size-[6px] rounded-full ring-1 ring-page-background",
              sandboxStatusDotClass(agent.status),
            )}
            aria-hidden="true"
          />
        </span>
        <span className="min-w-0 flex-1 truncate text-xs">{agent.task}</span>
        {live && agent.elapsedLabel && (
          <span className="shrink-0 text-[10px] text-muted-foreground">
            {agent.elapsedLabel}
          </span>
        )}
        {!live && <SandboxAgentStatusBadge status={agent.status} />}
      </SidebarButton>

      {live && onStop && (
        <WithTooltip label="Stop agent" side="right">
          <button
            type="button"
            className="absolute top-1/2 right-1.5 hidden -translate-y-1/2 rounded p-0.5 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive group-hover/agent:flex"
            aria-label={`Stop ${HARNESS_LABELS[agent.harness]} agent`}
            onClick={(event) => {
              event.stopPropagation();
              onStop(agent.id);
            }}
          >
            <Square className="size-3 fill-current" />
          </button>
        </WithTooltip>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Status badge (inline, minimal)
// ---------------------------------------------------------------------------

function SandboxAgentStatusBadge({ status }: { status: SandboxStatus }) {
  const variant = sandboxStatusBadgeVariant(status);
  return (
    <Badge variant={variant} size="sm" className="h-4 px-1.5 py-0 text-[10px]">
      {SANDBOX_STATUS_LABELS[status]}
    </Badge>
  );
}

// ---------------------------------------------------------------------------
// Detail row (expanded state for hover or panel)
// ---------------------------------------------------------------------------

export function SandboxAgentDetail({ agent }: { agent: SandboxAgent }) {
  const Icon = HARNESS_ICONS[agent.harness];

  return (
    <div className="flex flex-col gap-2 p-3">
      <div className="flex items-start gap-2.5">
        <div className="grid size-7 shrink-0 place-items-center">
          <Icon className="size-4" />
        </div>
        <div className="min-w-0 flex-1">
          <p className="line-clamp-2 text-sm font-medium leading-5">
            {agent.task}
          </p>
          <div className="mt-0.5 flex items-center gap-1.5 text-xs text-muted-foreground">
            <span>{HARNESS_LABELS[agent.harness]}</span>
            <span>·</span>
            <span>{agent.profile}</span>
            {agent.elapsedLabel && (
              <>
                <span>·</span>
                <span>{agent.elapsedLabel}</span>
              </>
            )}
          </div>
        </div>
        <SandboxAgentStatusBadge status={agent.status} />
      </div>

      {agent.repositoryUrl && (
        <div className="ml-9 flex items-center gap-1.5 text-xs text-muted-foreground">
          <GitBranch className="size-3 shrink-0" aria-hidden="true" />
          <span className="truncate">{repoShortName(agent.repositoryUrl)}</span>
          {agent.repositoryRef && (
            <>
              <span>@</span>
              <span className="truncate font-mono">{agent.repositoryRef}</span>
            </>
          )}
        </div>
      )}

      {agent.spendMicroUsd !== undefined && agent.spendMicroUsd > 0 && (
        <div className="ml-9 text-xs text-muted-foreground">
          ${(agent.spendMicroUsd / 1_000_000).toFixed(2)} spent
        </div>
      )}
    </div>
  );
}
