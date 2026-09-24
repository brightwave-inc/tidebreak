import type { Meta, StoryObj } from "@storybook/react-vite";

import { QueueTray, type QueueTrayApi, type QueueTrayRow } from "@/QueueTray";

/**
 * The durable message queue above the composer, shared by chat and code
 * sessions (decisions 9 and 69).
 *
 * Every row is a server-owned queued turn: edit, reorder, delete, and
 * send-now are real API calls behind the adapter, and the tray only observes.
 * Chat backs it with `/chats/{id}/queued`, a code session with
 * `/sessions/{id}/queued`; the rows render identically, which is the
 * point — a reader who learned the queue in one mode already knows the other.
 */
const meta = {
  title: "Composer/Queue tray",
  component: QueueTray,
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-3xl p-10">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof QueueTray>;

export default meta;
type Story = StoryObj<typeof meta>;

function staticQueue(rows: QueueTrayRow[], paused = false): QueueTrayApi {
  return {
    list: async () => ({ queued: rows, paused }),
    update: async () => undefined,
    remove: async () => undefined,
    setPaused: async () => undefined,
    sendNow: async () => undefined,
  };
}

const followUps: QueueTrayRow[] = [
  { id: "q-1", content: "Fix the failing checks on pull request #12" },
  { id: "q-2", content: "Address the review feedback, then push" },
  {
    id: "q-3",
    content:
      "Update the branch from main and rerun the focused tests before anything else lands on this workspace",
  },
];

/**
 * Three parked follow-ups behind a live turn. Hover a row for reorder, edit,
 * and delete; Send now moves it first and stops the running turn.
 */
export const Queued: Story = {
  args: {
    queue: staticQueue(followUps),
    active: true,
    onStop: async () => {},
  },
};

/**
 * A paused queue holds its rows and says so. Nothing promotes until the
 * reader resumes it or fires Send now.
 */
export const Paused: Story = {
  args: {
    queue: staticQueue(followUps.slice(0, 2), true),
    active: false,
    onStop: async () => {},
  },
};

/** One queued PR chore, the common case after a quick action mid-turn. */
export const SingleRow: Story = {
  args: {
    queue: staticQueue(followUps.slice(0, 1)),
    active: true,
    onStop: async () => {},
  },
};

/**
 * A trigger fire parked while the session was busy (decision 60 via decision
 * 69). The origin chip names the pull-request event, so the row reads as the
 * CI event it is rather than as something the person typed; edit, reorder,
 * and delete still work — the event is retractable like any queued turn.
 */
export const QueuedTriggerEvent: Story = {
  args: {
    queue: staticQueue([
      {
        id: "q-t1",
        origin: "Checks failed on #3411",
        content:
          "Tidebreak trigger: checks failed on #3411. Nobody typed this — a trigger you armed on this repository fired because the fact below changed.",
      },
      ...followUps.slice(0, 1),
    ]),
    active: true,
    onStop: async () => {},
  },
};

/**
 * Follow-ups that carry diff comments. The row counts the comments instead
 * of showing the block the agent reads; editing changes only the text, and
 * deleting the message puts its comments back in the review.
 */
export const QueuedReviewComments: Story = {
  args: {
    queue: staticQueue([
      {
        id: "q-r1",
        content: "Then rerun the queue tests.",
        reviewComments: {
          count: 3,
          withText: (text) => text,
          restore: () => {},
        },
      },
      {
        id: "q-r2",
        content: "",
        reviewComments: {
          count: 1,
          withText: (text) => text,
          restore: () => {},
        },
      },
    ]),
    active: true,
    onStop: async () => {},
  },
};

/**
 * An empty queue renders nothing at all — the composer stays untouched until
 * the first mid-turn send. The empty frame is here so a regression that
 * renders a bare header shows up.
 */
export const Empty: Story = {
  args: {
    queue: staticQueue([]),
    active: false,
    onStop: async () => {},
  },
};
