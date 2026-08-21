import { useNavigate } from "@tanstack/react-router";
import { formatDistanceToNow } from "date-fns";
import {
  CircleCheck,
  FolderOpen,
  ListChecks,
  MessageCircleQuestion,
  Save,
  ShieldQuestion,
  type LucideIcon,
} from "lucide-react";

import type { InboxItem, InboxItemKind } from "./api";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "./components/ui/empty";
import { useInbox } from "./Inbox";

/** How each kind reads, and what it is answered by. */
const KIND_PRESENTATION: Record<
  InboxItemKind,
  { label: string; description: string; icon: LucideIcon; iconClass: string }
> = {
  tool_approval: {
    label: "Approval",
    description: "An action is waiting for your decision.",
    icon: ShieldQuestion,
    iconClass: "text-icon-rose",
  },
  question: {
    label: "Question",
    description: "The assistant asked you something before continuing.",
    icon: MessageCircleQuestion,
    iconClass: "text-icon-cyan",
  },
  plan_review: {
    label: "Plan review",
    description: "A plan is waiting for you to accept or send back.",
    icon: ListChecks,
    iconClass: "text-icon-violet",
  },
  folder_access: {
    label: "Folder access",
    description: "A folder needs connecting before the work can go on.",
    icon: FolderOpen,
    iconClass: "text-icon-amber",
  },
  output_writeback: {
    label: "Save to folder",
    description: "A file is waiting for you to confirm the write.",
    icon: Save,
    iconClass: "text-icon-green",
  },
};

/**
 * Everything waiting on the reader, across their conversations.
 *
 * Nothing is answered here. An item is a pointer back to the card that owns the
 * decision, in the conversation it paused: that card is the only place with the
 * question, the plan, or the action under review, and routing every answer
 * through it is what keeps one resolution path rather than two. Opening an item
 * carries the parked call in the URL so the transcript reopens where it stopped.
 */
export function InboxView() {
  const navigate = useNavigate();
  const items = useInbox((state) => state.items);
  const loaded = useInbox((state) => state.loaded);

  if (items.length === 0) {
    return (
      <Empty className="h-full">
        <EmptyHeader>
          <EmptyMedia variant="icon" className="text-success">
            <CircleCheck aria-hidden="true" />
          </EmptyMedia>
          <EmptyTitle>{loaded ? "Nothing is waiting" : "Loading…"}</EmptyTitle>
          {loaded && (
            <EmptyDescription>
              Approvals, questions, and plan reviews from every conversation
              collect here while they wait for you.
            </EmptyDescription>
          )}
        </EmptyHeader>
      </Empty>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col gap-3 overflow-y-auto p-6">
      <h1 className="text-lg font-medium">Inbox</h1>
      <ul className="flex flex-col gap-2">
        {items.map((item) => (
          <li key={item.callId}>
            <InboxRow
              item={item}
              onOpen={() =>
                void navigate({
                  to: "/c/$chatId",
                  params: { chatId: item.chatId },
                  search: { focus: item.callId },
                })
              }
            />
          </li>
        ))}
      </ul>
    </div>
  );
}

function InboxRow({ item, onOpen }: { item: InboxItem; onOpen: () => void }) {
  const presentation = KIND_PRESENTATION[item.kind];
  const Icon = presentation.icon;
  const title = item.chatTitle?.trim() || "New work";
  return (
    <button
      type="button"
      onClick={onOpen}
      className="hover:bg-muted flex w-full items-center gap-3 rounded-lg border p-3 text-left"
    >
      <span className="flex size-8 shrink-0 items-center justify-center">
        <Icon
          aria-hidden="true"
          className={`${presentation.iconClass} size-5`}
        />
      </span>
      <span className="flex min-w-0 flex-col">
        <span className="flex min-w-0 items-center gap-2">
          <span className="text-sm font-medium">{presentation.label}</span>
          <span className="text-muted-foreground truncate text-sm">
            {title}
          </span>
        </span>
        <span className="text-muted-foreground text-xs">
          {presentation.description}
        </span>
      </span>
      <time
        dateTime={item.requestedAt}
        className="text-muted-foreground ml-auto shrink-0 text-xs"
      >
        {waitedFor(item.requestedAt)}
      </time>
    </button>
  );
}

/** How long this has been waiting, or nothing when the timestamp is unusable. */
export function waitedFor(requestedAt: string): string {
  const parsed = new Date(requestedAt);
  if (Number.isNaN(parsed.getTime())) return "";
  return `${formatDistanceToNow(parsed)} ago`;
}
