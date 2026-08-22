import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import type { CodeTriggerSnapshot } from "@/api/types";
import { CodeTriggerRules } from "@/code/CodeTriggerRules";

function trigger(
  condition: CodeTriggerSnapshot["condition"],
  action: CodeTriggerSnapshot["action"],
  enabled = true,
): CodeTriggerSnapshot {
  return {
    id: `trigger-${condition}`,
    repo_id: "repo-storybook",
    condition,
    action,
    enabled,
    created_at: "2026-08-22T00:00:00Z",
    updated_at: "2026-08-22T00:00:00Z",
  };
}

const meta = {
  title: "Code/Triggers",
  component: CodeTriggerRules,
  args: {
    triggers: [],
    target: {
      sessionTitle: "Fix the auth flow",
      harnessLabel: "Codex",
      delivery: "steer",
    },
    busy: false,
    onArm: fn(),
    onSetEnabled: fn(),
    onChangeAction: fn(),
  },
  decorators: [
    (Story) => (
      <div className="px-5 py-6">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof CodeTriggerRules>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Nothing armed yet: every condition off, every action control inert. */
export const NothingArmed: Story = {};

export const Armed: Story = {
  args: {
    triggers: [
      trigger("checks_failed", "deliver"),
      trigger("conflicts", "deliver"),
      trigger("ready_to_merge", "notify"),
    ],
  },
};

/**
 * A harness that cannot be interrupted. The copy has to say so rather than
 * implying every trigger reaches the agent mid-turn.
 */
export const WaitsForQuiet: Story = {
  args: {
    triggers: [trigger("checks_failed", "deliver")],
    target: {
      sessionTitle: "Rework the parser",
      harnessLabel: "Claude Code",
      delivery: "next_turn",
    },
  },
};

/** No session can receive a fire, so arming still works but nothing lands. */
export const NoTargetSession: Story = {
  args: {
    triggers: [trigger("checks_failed", "deliver")],
    target: null,
  },
};

/** A write is in flight; the controls refuse input rather than racing it. */
export const Busy: Story = {
  args: {
    triggers: [trigger("checks_failed", "deliver")],
    busy: true,
  },
};

/** Armed but switched off: the rule survives so its scoping is not rebuilt. */
export const Disabled: Story = {
  args: {
    triggers: [trigger("checks_failed", "deliver", false)],
  },
};
