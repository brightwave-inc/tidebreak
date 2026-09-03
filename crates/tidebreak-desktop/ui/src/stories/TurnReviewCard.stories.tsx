import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import { CodeTranscript } from "@/code/CodeTranscript";
import type { CodeTranscriptItem } from "@/code/CodeSessionReducer";
import { TurnReviewCard } from "@/code/TurnReviewCard";

const CODEX_REVOKED_TOKEN_ERROR =
  "Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.";

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

/** A recap is quiet supporting text beneath the completed-turn row. */
export const CompletedWithRecap: Story = {
  args: {
    recap:
      "Auth middleware is wired up and its tests pass. Next: hook the refresh path into the session store.",
  },
};

/**
 * With a fork handler, the seam carries a quiet actions menu: "Fork from
 * here" hands everything up to this turn to a fresh agent in a new tab.
 */
export const CompletedWithTurnActions: Story = {
  args: { onForkFromTurn: fn() },
};

/** A turn that changed nothing still says it finished, without a zero-stat chip. */
export const CompletedNoChanges: Story = {
  args: {
    turn: boundary({
      diffstat: { files: 0, insertions: 0, deletions: 0, truncated: false },
    }),
  },
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

const codexRevokedTokenItems: CodeTranscriptItem[] = [
  {
    kind: "notice",
    id: "notice-codex-auth",
    level: "error",
    message: CODEX_REVOKED_TOKEN_ERROR,
  },
  boundary({
    status: "failed",
    error: CODEX_REVOKED_TOKEN_ERROR,
    diffstat: null,
  }),
];

const renderCodexRevokedTokenTranscript = () => (
  <div className="h-[520px] min-h-0 overflow-auto">
    <CodeTranscript items={codexRevokedTokenItems} />
  </div>
);

/** The transcript folds the duplicate harness error into one recovery card. */
export const CodexRevokedRefreshToken: Story = {
  render: renderCodexRevokedTokenTranscript,
};

export const CodexRevokedRefreshTokenCompact: Story = {
  render: renderCodexRevokedTokenTranscript,
  parameters: { viewport: { defaultViewport: "compact" } },
};

/**
 * The engine refused a model its build predates. The card names the floor
 * and sends the reader to the update channel in Settings instead of
 * repeating the engine's own "run claude update", which a managed install
 * cannot follow.
 */
export const EngineTooOld: Story = {
  args: {
    turn: boundary({
      status: "failed",
      error:
        "API Error: 400 Claude Code 2.1.234 does not support this model; version 2.1.251 or newer is required. Run 'claude update', or update the Claude desktop app, then try again.",
      diffstat: null,
    }),
  },
};

/**
 * A failed turn is a prime fork point — retry the ask in a fresh context —
 * so the actions menu rides the failure card too.
 */
export const FailedWithTurnActions: Story = {
  args: {
    turn: boundary({
      status: "failed",
      error: "the engine exited before the turn completed",
      diffstat: null,
    }),
    onForkFromTurn: fn(),
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
