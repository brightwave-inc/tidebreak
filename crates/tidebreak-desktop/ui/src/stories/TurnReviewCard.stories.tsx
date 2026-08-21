import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { TurnReviewCard } from "@/code/TurnReviewCard";

function boundary(
  overrides: Partial<Parameters<typeof TurnReviewCard>[0]["turn"]> = {},
): Parameters<typeof TurnReviewCard>[0]["turn"] {
  return {
    kind: "turn_boundary",
    id: "boundary-turn-1",
    turnId: "turn-1",
    status: "completed",
    durationMs: 84_000,
    usage: {
      input_tokens: 48_213,
      output_tokens: 2_931,
      cache_read_input_tokens: 31_002,
      cache_creation_input_tokens: 0,
      context_tokens: 52_640,
    },
    error: null,
    diffstat: { files: 3, insertions: 96, deletions: 14, truncated: false },
    ...overrides,
  };
}

/**
 * The transcript's turn seam: how each engine turn ended, what it cost, and
 * what it changed. Failure and interruption must read differently from
 * success, and the diffstat is the door into the turn-scoped review.
 */
const meta = {
  title: "Code/Turn review card",
  component: TurnReviewCard,
  args: { turn: boundary(), onOpenTurnDiff: fn() },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof TurnReviewCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Completed: Story = {};

/** A turn that changed nothing still says it finished. */
export const CompletedNoChanges: Story = {
  args: { turn: boundary({ diffstat: null }) },
};

export const Failed: Story = {
  args: {
    turn: boundary({
      status: "failed",
      error: "the engine exited before the turn completed",
      diffstat: null,
    }),
  },
};

export const Interrupted: Story = {
  args: {
    turn: boundary({
      status: "interrupted",
      durationMs: 12_000,
      diffstat: { files: 1, insertions: 4, deletions: 0, truncated: false },
    }),
  },
};
