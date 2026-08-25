import type { ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";

import type { Attention, CodeSessionSnapshot } from "@/api";
import { AttentionBadge } from "@/code/AttentionBadge";
import { SessionLifecycleIndicator } from "@/code/SessionLifecycleIndicator";

import {
  attentionDoneUnreviewed,
  attentionFenced,
  attentionManual,
  attentionNeedsYou,
  attentionStalled,
  attentionWorking,
} from "./fixtures";

const attentionStates: Array<{
  label: string;
  description: string;
  attention: Attention;
}> = [
  {
    label: "Working",
    description: "The agent is doing work now.",
    attention: attentionWorking,
  },
  {
    label: "Needs you",
    description: "Only you can answer or approve the next step.",
    attention: attentionNeedsYou,
  },
  {
    label: "Stalled",
    description: "The session stopped making progress.",
    attention: attentionStalled,
  },
  {
    label: "Fenced",
    description: "The session cannot continue safely.",
    attention: attentionFenced,
  },
  {
    label: "Done",
    description: "The run ended and still needs review.",
    attention: attentionDoneUnreviewed,
  },
  {
    label: "Pinned",
    description: "You marked the session for a later look.",
    attention: attentionManual,
  },
];

const headerStates: Array<{
  label: string;
  description: string;
  lifecycle: CodeSessionSnapshot["lifecycle"];
  attention?: Attention;
  runningLabel?: string;
  unrecognizedEventCount?: number;
}> = [
  {
    label: "Live shell",
    description: "Motion means that the session is active now.",
    lifecycle: "running",
    runningLabel: "Shell running",
  },
  {
    label: "Needs you while running",
    description: "The alert outranks the live state without hiding it.",
    lifecycle: "running",
    attention: attentionNeedsYou,
    runningLabel: "Agent working",
  },
  {
    label: "Stalled",
    description: "The clock names a delay that needs a look.",
    lifecycle: "idle",
    attention: attentionStalled,
  },
  {
    label: "Fenced",
    description: "The blocked mark stays distinct from a temporary stall.",
    lifecycle: "fenced",
    attention: attentionFenced,
  },
  {
    label: "Transcript warning",
    description: "The trailing alert means that the adapter missed events.",
    lifecycle: "running",
    runningLabel: "Monitoring",
    unrecognizedEventCount: 3,
  },
  {
    label: "Ended",
    description: "A settled session uses quiet text and no motion.",
    lifecycle: "ended",
  },
];

function StatusRow({
  mark,
  label,
  description,
}: {
  mark: ReactNode;
  label: string;
  description: string;
}) {
  return (
    <div className="grid grid-cols-[5rem_minmax(0,1fr)] items-center gap-4 border-t border-border-subtle px-4 py-3 first:border-t-0">
      <div className="flex min-h-7 items-center gap-2">{mark}</div>
      <div className="min-w-0">
        <p className="text-sm font-medium">{label}</p>
        <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>
      </div>
    </div>
  );
}

function StateIndicators() {
  return (
    <main className="min-h-screen bg-page-background px-6 py-10 text-foreground">
      <div className="mx-auto grid max-w-4xl gap-8">
        <header className="max-w-2xl">
          <p className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
            Code mode
          </p>
          <h1 className="mt-2 text-2xl font-semibold tracking-tight">
            State indicators
          </h1>
          <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
            Shape carries the state. Color reinforces it. Motion appears only
            while work is happening now.
          </p>
        </header>

        <section className="grid gap-3">
          <div>
            <h2 className="text-base font-semibold">Compact attention marks</h2>
            <p className="mt-1 text-sm text-muted-foreground">
              Workspace cards, tabs, and rails use these marks at high density.
            </p>
          </div>
          <div className="overflow-hidden rounded-xl border border-border-subtle bg-background">
            {attentionStates.map((state) => (
              <StatusRow
                key={state.label}
                mark={<AttentionBadge attention={state.attention} compact />}
                label={state.label}
                description={state.description}
              />
            ))}
          </div>
        </section>

        <section className="grid gap-3">
          <div>
            <h2 className="text-base font-semibold">
              Workspace header combinations
            </h2>
            <p className="mt-1 text-sm text-muted-foreground">
              The header combines live work, attention, and transcript fidelity
              without turning each fact into a pill.
            </p>
          </div>
          <div className="overflow-hidden rounded-xl border border-border-subtle bg-background">
            {headerStates.map((state) => (
              <StatusRow
                key={state.label}
                mark={
                  <>
                    {state.attention && (
                      <AttentionBadge attention={state.attention} compact />
                    )}
                    <SessionLifecycleIndicator
                      lifecycle={state.lifecycle}
                      harness="codex"
                      version="0.84.0"
                      unrecognizedEventCount={state.unrecognizedEventCount ?? 0}
                      runningLabel={state.runningLabel}
                    />
                  </>
                }
                label={state.label}
                description={state.description}
              />
            ))}
          </div>
        </section>
      </div>
    </main>
  );
}

const meta = {
  title: "Code/State indicators",
  component: StateIndicators,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof StateIndicators>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Catalog: Story = {};
