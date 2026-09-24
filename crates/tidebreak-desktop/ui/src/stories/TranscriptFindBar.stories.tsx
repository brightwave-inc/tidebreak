import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState, type ReactNode } from "react";
import { expect, fn, userEvent, within } from "storybook/test";

import { TranscriptFindBar } from "@/search/TranscriptFindBar";
import type { TranscriptFindState } from "@/search/useTranscriptFind";
import { UserMessage } from "@/UserMessage";
import { messageSearchHit, messageSearchIndexed } from "./fixtures";

/**
 * Find in the open conversation (Cmd+F). The bar floats over the
 * transcript's top edge, the way a browser's find bar does, so opening it
 * moves nothing the reader was looking at.
 *
 * Matches run newest first: the first one shown is nearest the reader, Enter
 * steps up to the next older match and Shift+Enter back down, and the count
 * covers the whole conversation, pages the transcript has not loaded
 * included.
 */

const matches = Array.from({ length: 12 }, (_, index) =>
  messageSearchHit(`harbour note ${index}`, "harbour", {
    message_id: `m${index}`,
  }),
);

function state(
  overrides: Partial<TranscriptFindState> = {},
): TranscriptFindState {
  return {
    status: "ready",
    query: "harbour",
    matches,
    capped: false,
    position: 0,
    indexing: messageSearchIndexed,
    error: null,
    ...overrides,
  };
}

/** The transcript the bar floats over, so it is judged in place. */
function TranscriptBehind({ children }: { children: ReactNode }) {
  return (
    <div className="message-view relative min-h-[22rem] bg-background">
      <div className="pointer-events-none absolute inset-x-0 top-2 z-[3] flex justify-end px-3">
        {children}
      </div>
      <div className="messages">
        <div className="messages-column">
          <UserMessage text="What did we decide about the harbour fees?" />
          <article className="message message-assistant" aria-label="Assistant">
            <p className="text-md leading-relaxed">
              The harbour dues stay as they are. The overnight berth surcharge
              goes up in October, and the notice goes out in the harbour
              bulletin two weeks before.
            </p>
          </article>
          <UserMessage text="Draft the notice for the bulletin." />
        </div>
      </div>
    </div>
  );
}

const meta = {
  component: TranscriptFindBar,
  title: "Search/Find bar",
  args: {
    query: "harbour",
    onQueryChange: fn(),
    onOlder: fn(),
    onNewer: fn(),
    onClose: fn(),
    className: "pointer-events-auto",
    state: state(),
  },
  decorators: [
    (Story) => (
      <TranscriptBehind>
        <Story />
      </TranscriptBehind>
    ),
  ],
} satisfies Meta<typeof TranscriptFindBar>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Twelve messages match; the newest is on screen. */
export const Matches: Story = {};

/** Nothing in the conversation matches; the steps have nowhere to go. */
export const NoMatches: Story = {
  args: {
    query: "lighthouse",
    state: state({ query: "lighthouse", matches: [], position: -1 }),
  },
};

/** The search is out. */
export const Searching: Story = {
  args: { state: state({ status: "loading", matches: [], position: -1 }) },
};

/** More matched than the bar steps through. */
export const Capped: Story = {
  args: { state: state({ capped: true }) },
};

/** The conversation is still being added to the index. */
export const StillIndexing: Story = {
  args: {
    state: state({
      indexing: {
        complete: false,
        pending_conversations: 1,
        failed_conversations: 0,
      },
    }),
  },
};

/** The search failed. */
export const SearchFailed: Story = {
  args: {
    state: state({
      status: "error",
      matches: [],
      position: -1,
      error: "Could not search. The server did not answer.",
    }),
  },
};

/** Enter steps up the conversation and Shift+Enter back down, wrapping. */
export const Stepping: Story = {
  render: function Stepping(args) {
    const [current, setCurrent] = useState(args.state);
    const step = (delta: number) =>
      setCurrent((previous) => ({
        ...previous,
        position:
          (previous.position + delta + previous.matches.length) %
          previous.matches.length,
      }));
    return (
      <TranscriptFindBar
        {...args}
        state={current}
        onOlder={() => step(1)}
        onNewer={() => step(-1)}
      />
    );
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const field = canvas.getByRole("textbox", {
      name: "Find in conversation",
    });
    await userEvent.click(field);
    await userEvent.keyboard("{Enter}{Enter}{Enter}");
    await userEvent.keyboard("{Shift>}{Enter}{/Shift}");
    await expect(canvas.getByText("3 of 12")).toBeInTheDocument();
  },
};
