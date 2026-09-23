import { toast } from "sonner";

import type { AgentNotification } from "./api";
import { rememberBannerTarget } from "./bannerTarget";
import {
  isWindowFocused,
  presentNativeNotification,
  requestUserAttention,
} from "./host";
import { needsYouTitle, plainText, type NeedsYouQuestion } from "./needsYou";
import {
  notificationHref,
  notificationPresent,
  type NotificationPresentKind,
} from "./notificationPresent";
import {
  finishedNotificationsEnabled,
  needsYouNotificationsEnabled,
} from "./NotificationPreferences";

/**
 * How Tidebreak tells you about agent work: the decision, then the toast,
 * banner, or Dock bounce it picked. Both kinds of notice go through here, so
 * they cannot drift apart on when to stay quiet.
 */

/** Open a route from a toast's action. Hash history owns the URL. */
function openRoute(href: string): void {
  window.location.hash = `#${href}`;
}

/** An agent that stopped for you, in a conversation you are not looking at. */
export type NeedsYouNotice = {
  /** The conversation's name, as the rail shows it. */
  name: string;
  /** Where the conversation opens. */
  href: string;
  /** Whether the conversation is on screen right now. */
  viewing: boolean;
  /**
   * What the agent asked. Read only when a notice is shown, because it costs
   * a request and most parks happen in the conversation you are reading.
   */
  question: () => Promise<NeedsYouQuestion>;
  /** Whether the conversation still waits on you. Guards the banner target. */
  stillWaiting: () => boolean;
};

/**
 * Tell you an agent is blocked on you.
 *
 * In the background, this posts a banner and bounces the Dock icon until you
 * come back: a blocked agent does no work until you answer.
 */
export async function presentNeedsYou(
  notice: NeedsYouNotice,
): Promise<NotificationPresentKind> {
  const windowFocused = await isWindowFocused();
  const kind = notificationPresent({
    windowFocused,
    viewingConversation: notice.viewing,
    permission: windowFocused ? "granted" : "prompt",
    enabled: needsYouNotificationsEnabled(),
  });
  if (kind === "skip") return kind;
  const question = await notice
    .question()
    .catch((): NeedsYouQuestion => ({ kind: "approval", text: "" }));
  const title = needsYouTitle(question.kind, notice.name);
  if (kind === "toast") {
    toast(title, {
      description: question.text || undefined,
      action: { label: "Open", onClick: () => openRoute(notice.href) },
    });
    return kind;
  }
  void requestUserAttention({ critical: true }).catch(() => {});
  if (
    kind === "native" &&
    (await presentNativeNotification(title, question.text))
  ) {
    rememberBannerTarget(notice.href, notice.stillWaiting);
  }
  return kind;
}

/**
 * Tell you an agent finished or failed, with the row's body under its title.
 *
 * `onOpen` marks the row read when you open it from a toast.
 */
export async function presentFinished(
  row: AgentNotification,
  input: {
    viewing: boolean;
    stillUnread: () => boolean;
    onOpen: () => void;
  },
): Promise<NotificationPresentKind> {
  const windowFocused = await isWindowFocused();
  const kind = notificationPresent({
    windowFocused,
    viewingConversation: input.viewing,
    permission: windowFocused ? "granted" : "prompt",
    enabled: finishedNotificationsEnabled(),
  });
  if (kind === "skip") return kind;
  const href = notificationHref(row);
  const title = plainText(row.title);
  const body = plainText(row.body ?? "");
  if (kind === "toast") {
    toast(title, {
      description: body || undefined,
      action: {
        label: "Open",
        onClick: () => {
          openRoute(href);
          input.onOpen();
        },
      },
    });
    return kind;
  }
  if (kind === "native" && (await presentNativeNotification(title, body))) {
    rememberBannerTarget(href, input.stillUnread);
    return kind;
  }
  void requestUserAttention().catch(() => {});
  return kind;
}
