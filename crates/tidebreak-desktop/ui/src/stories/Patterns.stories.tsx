import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import {
  Bot,
  CircleAlert,
  CircleCheck,
  FileText,
  FolderOpen,
  Globe,
  Shield,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ConfirmDialog";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { ToolCardShell } from "@/ToolCardShell";
import { ApprovalCard } from "@/ApprovalCard";
import { ChatStatusChip } from "@/ChatStatusChip";
import { SettingsSection, SettingsField } from "@/settings/primitives";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { execPreview } from "./fixtures";

function SettingsComposition() {
  const { confirm, dialog } = useConfirm();
  return (
    <>
      <SettingsSection
        title="General"
        description="Name and behavior for this workspace."
      >
        <SettingsField label="Workspace name">
          <Input defaultValue="Acme Labs" />
        </SettingsField>
        <SettingsField
          label="Automatic backups"
          hint="Nightly snapshots of every project in this workspace."
        >
          <Switch defaultChecked />
        </SettingsField>
      </SettingsSection>
      <SettingsSection title="Danger zone" description="Irreversible actions.">
        <div className="flex items-center justify-between gap-4">
          <div>
            <p className="text-sm font-medium">Delete this workspace</p>
            <p className="text-xs text-muted-foreground">
              Permanently removes Acme Labs and all of its projects.
            </p>
          </div>
          <Button
            variant="destructive"
            size="sm"
            onClick={() =>
              void confirm({
                title: "Delete this workspace?",
                description:
                  "Tidebreak removes Acme Labs and all of its projects. This cannot be undone.",
                confirmLabel: "Delete workspace",
                destructive: true,
              })
            }
          >
            Delete workspace
          </Button>
        </div>
      </SettingsSection>
      {dialog}
    </>
  );
}

function PatternsShowcase() {
  return (
    <div className="mx-auto grid w-full max-w-5xl gap-12 p-8">
      <section className="grid gap-4">
        <div className="max-w-xl">
          <h2 className="text-base font-semibold tracking-tight">
            Settings composition
          </h2>
          <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
            A settings page is a stack of sections on a bounded column that
            SettingsPanel owns. Each section groups related fields. Each field
            is a label and its control. The page saves on change; there are no
            Save or Cancel buttons. Destructive actions go through the shared
            confirm dialog.
          </p>
        </div>
        <div className="max-w-2xl">
          <SettingsComposition />
        </div>
      </section>

      <section className="grid gap-4">
        <div className="max-w-xl">
          <h2 className="text-base font-semibold tracking-tight">
            Tool call row
          </h2>
          <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
            The canonical expandable row for a tool call. Collapsed it is a
            single line with a small icon; expanded it shows the command,
            output, and metadata. Expand only while work is still happening; a
            settled card collapses to a single row.
          </p>
        </div>
        <div className="grid max-w-prose gap-3">
          <div>
            <p className="text-xs font-medium text-muted-foreground">
              Running (expanded on mount)
            </p>
            <ToolCardShell
              icon={<FileText className="size-3.5" aria-hidden="true" />}
              title="pnpm test -- TaskPlanCard.dom.test.tsx"
              label="Run the focused component tests"
              badge={
                <>
                  <Badge variant="outline">exec</Badge>
                  <Badge variant="info">Running</Badge>
                </>
              }
              defaultExpanded
            >
              <div className="rounded-md bg-muted p-3 text-xs text-muted-foreground">
                <pre className="whitespace-pre-wrap">
                  {`Running the focused component tests…

$ pnpm test -- TaskPlanCard.dom.test.tsx`}
                </pre>
              </div>
            </ToolCardShell>
          </div>
          <div>
            <p className="text-xs font-medium text-muted-foreground">
              Failed (collapsed)
            </p>
            <ToolCardShell
              icon={<FileText className="size-3.5" aria-hidden="true" />}
              title="Checking the PowerPoint library"
              label="Run a command: Tool could not complete"
              badge={
                <>
                  <Badge variant="outline">Local</Badge>
                  <Badge variant="outline" className="text-destructive">
                    Exit 1
                  </Badge>
                </>
              }
              trailing={
                <CircleAlert className="text-muted-foreground size-3.5" />
              }
            >
              <div className="rounded-md bg-muted p-3 text-xs text-muted-foreground">
                <pre className="whitespace-pre-wrap">
                  {`Error: Cannot find module 'pptxgenjs'`}
                </pre>
              </div>
            </ToolCardShell>
          </div>
          <div>
            <p className="text-xs font-medium text-muted-foreground">
              Settled (collapsed)
            </p>
            <ToolCardShell
              icon={<FileText className="size-3.5" aria-hidden="true" />}
              title="pnpm test -- TaskPlanCard.dom.test.tsx"
              label="Run the focused component tests"
              badge={
                <>
                  <Badge variant="outline">exec</Badge>
                  <Badge variant="success">Exit 0</Badge>
                </>
              }
              trailing="1.2s"
            >
              <div className="rounded-md bg-muted p-3 text-xs text-muted-foreground">
                <pre className="whitespace-pre-wrap">
                  {`$ pnpm test -- TaskPlanCard.dom.test.tsx

 ✓ TaskPlanCard renders the active step (12 ms)
 ✓ TaskPlanCard collapses settled steps (8 ms)

 Test Files  1 passed (1)
      Tests  2 passed (2)`}
                </pre>
              </div>
            </ToolCardShell>
          </div>
        </div>
      </section>

      <section className="grid gap-4">
        <div className="max-w-xl">
          <h2 className="text-base font-semibold tracking-tight">
            Approval card
          </h2>
          <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
            The canonical consent surface. It leads with a short question, shows
            the exact action in a muted preview block, and lists choices as
            numbered rows ordered narrowest grant first.
          </p>
        </div>
        <div className="max-w-2xl">
          <ApprovalCard
            callId="pattern-storybook"
            summary="This command can access the network and the staged files listed below."
            preview={execPreview}
            canApprove
            canRemember
            grantRungs={["exact_action", "whole_tool"]}
            deciding={false}
            onDecide={fn()}
          />
        </div>
      </section>

      <section className="grid gap-4">
        <div className="max-w-xl">
          <h2 className="text-base font-semibold tracking-tight">
            Activity summary
          </h2>
          <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
            The conversation header shows live work first, outputs otherwise.
            The compact pill collapses when a side panel needs the canvas.
          </p>
        </div>
        <div className="flex flex-wrap items-start gap-6">
          <div className="grid gap-2">
            <p className="text-xs font-medium text-muted-foreground">
              Expanded card
            </p>
            <ChatStatusChip
              outputCount={3}
              folders={[]}
              runs={[
                {
                  id: "run-1",
                  parent_id: "parent",
                  spawn_call_id: "spawn-1",
                  tier: "background",
                  execution_location: "in_process",
                  code_execution_provider: "local",
                  status: "running",
                  model_steps: 3,
                  usage: {
                    input_tokens: 1000,
                    output_tokens: 200,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                  },
                  task: "Audit the conversation flow",
                  started_at: "2026-08-24T13:00:00Z",
                  finished_at: null,
                  last_error_code: null,
                  activity: { kind: "exec", status: "running" },
                  submitted_outputs: [],
                  terminal_text: null,
                  created_at: "2026-08-24T13:00:00Z",
                  updated_at: "2026-08-24T13:00:00Z",
                },
              ]}
              onOpenOutputs={fn()}
              onOpenFolders={fn()}
              onOpenPermissions={fn()}
              onOpenAgents={fn()}
            />
          </div>
          <div className="grid gap-2">
            <p className="text-xs font-medium text-muted-foreground">
              Compact pill
            </p>
            <ChatStatusChip
              compact
              outputCount={0}
              folders={[]}
              runs={[]}
              onOpenOutputs={fn()}
              onOpenFolders={fn()}
              onOpenPermissions={fn()}
              onOpenAgents={fn()}
            />
          </div>
        </div>
      </section>

      <section className="grid gap-4">
        <div className="max-w-xl">
          <h2 className="text-base font-semibold tracking-tight">
            Empty states
          </h2>
          <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
            An empty index renders an icon in muted or identity ink at size-11
            with no filled container behind it. The same variant is how Inbox,
            Folders, Outputs, Plugins, Apps, and the Code pages read when they
            have nothing to list. In high-density operational surfaces, icons
            stay small and unboxed.
          </p>
        </div>
        <div className="grid gap-6 md:grid-cols-2">
          <div className="grid gap-2">
            <p className="text-xs font-medium text-muted-foreground">
              Empty index
            </p>
            <Empty className="min-h-48 bg-background ring-1 ring-foreground/10">
              <EmptyHeader>
                <EmptyMedia variant="icon" className="text-icon-amber">
                  <FolderOpen aria-hidden="true" />
                </EmptyMedia>
                <EmptyTitle>No folders connected</EmptyTitle>
                <EmptyDescription>
                  Connect a folder so the agent can read and write files in it.
                </EmptyDescription>
              </EmptyHeader>
              <EmptyContent>
                <Button variant="outline">Connect a folder</Button>
              </EmptyContent>
            </Empty>
          </div>
          <div className="grid gap-2">
            <p className="text-xs font-medium text-muted-foreground">
              High density (menus, tables, rows)
            </p>
            <div className="rounded-lg border border-border bg-background p-2">
              <div className="flex flex-col gap-0.5">
                {[
                  { icon: FileText, label: "Read a file", time: "2m ago" },
                  { icon: Globe, label: "Search the web", time: "5m ago" },
                  { icon: Shield, label: "Request access", time: "12m ago" },
                ].map(({ icon: Icon, label, time }) => (
                  <button
                    key={label}
                    type="button"
                    className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm hover:bg-muted/50"
                  >
                    <Icon
                      className="size-3.5 shrink-0 text-muted-foreground"
                      aria-hidden="true"
                    />
                    <span className="flex-1">{label}</span>
                    <span className="text-xs text-muted-foreground">
                      {time}
                    </span>
                  </button>
                ))}
              </div>
            </div>
          </div>
        </div>
      </section>

      <section className="grid gap-4">
        <div className="max-w-xl">
          <h2 className="text-base font-semibold tracking-tight">
            Status vocabulary
          </h2>
          <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
            Six tones cover every outcome and state. Use the Badge variants;
            never pick raw palette classes.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="success">
            <CircleCheck className="size-3" aria-hidden="true" /> Ready
          </Badge>
          <Badge variant="warning">
            <CircleAlert className="size-3" aria-hidden="true" /> Waiting
          </Badge>
          <Badge variant="critical">
            <CircleAlert className="size-3" aria-hidden="true" /> Failed
          </Badge>
          <Badge variant="info">
            <Bot className="size-3" aria-hidden="true" /> Running
          </Badge>
          <Badge variant="merged">Merged</Badge>
          <Badge variant="live">Working</Badge>
          <Badge variant="outline">Neutral</Badge>
        </div>
      </section>
    </div>
  );
}

const meta = {
  title: "Foundations/Patterns",
  component: PatternsShowcase,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof PatternsShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

export const CanonicalPatterns: Story = {};
