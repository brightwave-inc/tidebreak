import type { Meta, StoryObj } from "@storybook/react-vite";
import { userEvent, within } from "storybook/test";

import { TriggerEventCard } from "@/code/TriggerEventCard";
import type { TriggerTurnContext } from "@/generated/wire";

/**
 * A pull-request event delivered to the agent in a conversation: a trigger
 * fire or a watch fix turn, drawn as the event it is rather than as a
 * person's message. The collapsed row leads with the condition and the pull
 * request; the expand holds the failing checks, the head, and the exact
 * instruction the agent received. The states that matter are the conditions
 * with different tones, the watch source, and a long title forcing truncation.
 */
const meta = {
  title: "Code/Trigger event",
  component: TriggerEventCard,
  args: {
    context: checksFailed(),
    // The server renders paragraphs as single long lines; only the list
    // entries carry their own newlines. The fixture matches that shape so
    // the story exercises real wrapping, not pre-broken lines.
    message: [
      "Tidebreak trigger: checks failed on #3411. Nobody typed this — a trigger you armed on this repository fired because the fact below changed.",
      "",
      "Pull request: #3411 - Bound every string the code parser draws",
      "URL: https://github.com/acme/tidebreak/pull/3411",
      "Failing checks:",
      "- desktop-ui (https://github.com/acme/tidebreak/actions/runs/1)",
      "- cargo test / linux (https://github.com/acme/tidebreak/actions/runs/2)",
      "",
      "Decide whether to act on this now. Do not merge, enable auto-merge, or change the pull request's draft or review state — those stay the user's.",
    ].join("\n"),
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof TriggerEventCard>;

export default meta;
type Story = StoryObj<typeof meta>;

function checksFailed(): TriggerTurnContext {
  return {
    source: "trigger",
    condition: "checks_failed",
    pr_number: 3411,
    pr_title: "Bound every string the code parser draws",
    pr_url: "https://github.com/acme/tidebreak/pull/3411",
    head_sha: "0f0e0d0c0b0a09081716151413121110",
    failing_checks: [
      {
        name: "desktop-ui",
        bucket: "fail",
        url: "https://github.com/acme/tidebreak/actions/runs/1",
      },
      {
        name: "cargo test / linux",
        bucket: "fail",
        url: "https://github.com/acme/tidebreak/actions/runs/2",
      },
    ],
  };
}

export const ChecksFailed: Story = {};

/** The same event opened: checks, head, and the delivered instruction. */
export const ChecksFailedExpanded: Story = {
  play: async ({ canvasElement }) => {
    await userEvent.click(within(canvasElement).getByRole("button"));
  },
};

/** A watch fix turn wears the same card with its own source label. */
export const WatchConflicts: Story = {
  args: {
    context: {
      source: "watch",
      condition: "conflicts",
      pr_number: 3398,
      pr_title: "Collapse replaced grant history",
      pr_url: "https://github.com/acme/tidebreak/pull/3398",
      head_sha: "77aa66bb55cc44dd33ee22ff11009988",
      failing_checks: [],
    },
    message:
      "Watch: #3398 has merge conflicts with its base. Rebase onto the base " +
      "branch, resolve the conflicts, and push. Do not merge or change the " +
      "pull request's review state.",
  },
};

/** A reviewer verdict is a warning, not a failure. */
export const ChangesRequested: Story = {
  args: {
    context: {
      source: "trigger",
      condition: "changes_requested",
      pr_number: 3402,
      pr_title: "Name channel grants in settings",
      pr_url: "https://github.com/acme/tidebreak/pull/3402",
      failing_checks: [],
    },
    message:
      "Tidebreak trigger: changes requested on #3402. Read the review, " +
      "address each finding or say why not, and push.",
  },
};

/** Good news arrives the same way: ready to merge stays the user's call. */
export const ReadyToMerge: Story = {
  args: {
    context: {
      source: "trigger",
      condition: "ready_to_merge",
      pr_number: 3402,
      pr_title: "Name channel grants in settings",
      pr_url: "https://github.com/acme/tidebreak/pull/3402",
      failing_checks: [],
    },
    message:
      "Tidebreak trigger: #3402 is ready to merge. Nothing is outstanding. " +
      "Merging stays the user's; summarize the state for them.",
  },
};

/** A long title truncates; the condition and number never do. */
export const LongTitle: Story = {
  args: {
    context: {
      ...checksFailed(),
      pr_title:
        "Rework the delivery monitor so hosted conversation rows stop " +
        "reloading the rail on every digest and the card metadata matches " +
        "the workspace column exactly",
    },
  },
};

/** No context beyond the condition: the card still stands on its own. */
export const MinimalContext: Story = {
  args: {
    context: {
      source: "trigger",
      condition: "pr_updated",
      pr_number: 3405,
      failing_checks: [],
    },
    message: "Tidebreak trigger: #3405 has a new head.",
  },
};
