import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, waitFor, within } from "storybook/test";
import type { Attention, FenceReason } from "@/api/types";
import { recoveryAttention } from "@/code/sessionRecovery";
import { SessionRecoveryNotice } from "@/code/SessionRecoveryNotice";
import { SessionLifecycleIndicator } from "@/code/SessionLifecycleIndicator";
import { attentionFenced } from "./fixtures";

const meta = {
  title: "Code/Session recovery",
  component: SessionRecoveryNotice,
  parameters: { layout: "padded" },
} satisfies Meta<typeof SessionRecoveryNotice>;
export default meta;
type Story = StoryObj<typeof meta>;

const blockers: Array<{ title: string; reason: FenceReason; prompt: string }> =
  [
    {
      title: "Sign-in required",
      reason: {
        type: "repeated_turn_failures",
        count: 3,
        detail: "Authentication failed",
      },
      prompt:
        "The session stopped after repeated authentication failures. Sign in to the engine, then retry recovery.",
    },
    {
      title: "Process needs inspection",
      reason: { type: "probe_ambiguous", detail: "Process identity changed" },
      prompt:
        "The previous process could not be identified safely. Inspect the process before retrying recovery.",
    },
    {
      title: "Final output unavailable",
      reason: {
        type: "terminal_flush_missing",
        detail: "Final output unavailable",
      },
      prompt:
        "The remote session ended without final output. Inspect the saved transcript before continuing.",
    },
  ];

export const States: Story = {
  args: { onRetry: () => {}, lifecycle: "fenced", attention: attentionFenced },
  render: () => (
    <div className="flex max-w-xl flex-col gap-6">
      <section className="flex flex-col gap-2">
        <h2 className="text-sm font-medium">Reconnecting</h2>
        <SessionLifecycleIndicator
          lifecycle="fenced"
          attention={attentionFenced}
          harness="codex"
          unrecognizedEventCount={0}
        />
        <SessionRecoveryNotice
          showProgress={false}
          lifecycle="fenced"
          attention={attentionFenced}
          onRetry={() => {}}
        />
      </section>
      {blockers.map(({ title, reason, prompt }) => {
        const attention: Attention =
          reason.type === "probe_ambiguous"
            ? {
                state: { type: "manual", note: "Review today" },
                source: "user",
              }
            : {
                state: { type: "needs_you", source: "lifecycle", prompt },
                source: "lifecycle",
              };
        return (
          <section key={title} className="flex flex-col gap-2">
            <h2 className="text-sm font-medium">{title}</h2>
            <SessionLifecycleIndicator
              lifecycle="fenced"
              attention={recoveryAttention("fenced", attention, reason)}
              harness="codex"
              unrecognizedEventCount={0}
            />
            <SessionRecoveryNotice
              lifecycle="fenced"
              attention={attention}
              reason={reason}
              onRetry={() => {}}
            />
          </section>
        );
      })}
      <section className="flex flex-col gap-2">
        <h2 className="text-sm font-medium">Recovered</h2>
        <SessionLifecycleIndicator
          lifecycle="idle"
          attention={{ state: { type: "idle" }, source: "lifecycle" }}
          harness="codex"
          unrecognizedEventCount={0}
        />
        <SessionRecoveryNotice
          lifecycle="idle"
          attention={{ state: { type: "idle" }, source: "lifecycle" }}
          onRetry={() => {}}
        />
      </section>
    </div>
  ),
  play: async ({ canvasElement }) => {
    await waitFor(
      () =>
        expect(
          within(canvasElement).getAllByText("Reconnecting…"),
        ).toHaveLength(1),
      { timeout: 3000 },
    );
  },
};
