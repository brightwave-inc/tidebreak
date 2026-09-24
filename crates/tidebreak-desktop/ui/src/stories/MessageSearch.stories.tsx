import type { Meta, StoryObj } from "@storybook/react-vite";
import { MessageSquare, Plus, Search } from "lucide-react";
import { fn } from "storybook/test";

import { rankPaletteRows, type PaletteRow } from "@/CommandPalette";
import { CommandPaletteList } from "@/CommandPaletteList";
import type { MessageSearchState } from "@/search/useMessageSearch";
import {
  archivedMessageSearchHit,
  longSnippetMessageSearchHit,
  messageSearchHits,
  messageSearchIndexed,
  messageSearchNow,
} from "./fixtures";

/**
 * The palette's Messages section: typing searches what was said in every
 * chat and code session, beside the commands and conversations the palette
 * already ranks.
 *
 * The section's every state is a row inside it, so nothing above it moves
 * while a search is out. Times are fixed against `messageSearchNow`, so the
 * stories read the same whenever they are opened.
 */

const rows: PaletteRow[] = [
  {
    id: "chat:c-1",
    section: "chats",
    label: "Harbour pricing notes",
    hint: "Marketing",
    icon: MessageSquare,
    onSelect: fn(),
  },
  {
    id: "conversation:find",
    section: "actions",
    label: "Find in this conversation",
    icon: Search,
    shortcut: "find-in-transcript",
    onSelect: fn(),
  },
  {
    id: "navigate:new-chat",
    section: "actions",
    label: "Start new work",
    icon: Plus,
    shortcut: "new-chat",
    onSelect: fn(),
  },
];

function ready(
  overrides: Partial<MessageSearchState> = {},
): MessageSearchState {
  return {
    status: "ready",
    query: "harbour",
    hits: messageSearchHits,
    indexing: messageSearchIndexed,
    error: null,
    ...overrides,
  };
}

const meta = {
  component: CommandPaletteList,
  title: "Search/Messages in the palette",
  args: {
    query: "harbour",
    onQueryChange: fn(),
    onSelect: fn(),
    onSelectMessage: fn(),
    command: true,
    mode: "chat",
    groups: rankPaletteRows(rows, "harbour"),
    messages: ready(),
    now: messageSearchNow,
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl overflow-hidden rounded-xl border border-border-subtle bg-popover shadow-2xl">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof CommandPaletteList>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Hits from a chat and a code session, the matched words marked. */
export const Results: Story = {};

/** The first search of a query is out; the rows above it stay where they are. */
export const Loading: Story = {
  args: { messages: ready({ status: "loading", hits: [] }) },
};

/**
 * A newer query is out while the last one's hits are still up, so the list
 * does not blink empty between words.
 */
export const Refreshing: Story = {
  args: { messages: ready({ status: "loading" }) },
};

/** Nothing matched, said in the reader's own words. */
export const Empty: Story = {
  args: {
    query: "lighthouse",
    groups: rankPaletteRows(rows, "lighthouse"),
    messages: ready({ query: "lighthouse", hits: [] }),
  },
};

/** The search failed; the commands still answer. */
export const SearchFailed: Story = {
  args: {
    messages: ready({
      status: "error",
      hits: [],
      error: "Could not search messages. The server did not answer.",
    }),
  },
};

/** Older conversations are still being added, so a miss is not final. */
export const Indexing: Story = {
  args: {
    messages: ready({
      hits: messageSearchHits.slice(0, 1),
      indexing: {
        complete: false,
        pending_conversations: 12,
        failed_conversations: 0,
      },
    }),
  },
};

/** A quiet note for conversations the index gave up on. */
export const IndexFailed: Story = {
  args: {
    messages: ready({
      indexing: {
        complete: false,
        pending_conversations: 0,
        failed_conversations: 2,
      },
    }),
  },
};

/** A snippet cut on both sides, under a title too long for the row. */
export const LongSnippets: Story = {
  args: {
    messages: ready({
      hits: [longSnippetMessageSearchHit, ...messageSearchHits.slice(0, 1)],
    }),
  },
};

/** An archived conversation stays searchable, and says it is archived. */
export const Archived: Story = {
  args: {
    messages: ready({
      hits: [archivedMessageSearchHit, ...messageSearchHits.slice(0, 1)],
    }),
  },
};
