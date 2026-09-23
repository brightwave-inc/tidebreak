import { useNavigate, useRouterState } from "@tanstack/react-router";
import { Inbox } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { useInbox } from "@/Inbox";
import { SidebarButton } from "./primitives";

/** Where the inbox opens from each mode, so opening it keeps your rail. */
export function inboxPath(pathname: string): "/inbox" | "/code/inbox" {
  return pathname === "/code" || pathname.startsWith("/code/")
    ? "/code/inbox"
    : "/inbox";
}

/**
 * The way to everything that is waiting, from anywhere.
 *
 * It sits on both rails because being blocked is not scoped to a conversation
 * or a mode: you are most likely to be inside one chat or workspace when
 * another one parks. Each mode opens the same inbox under its own rail. The
 * count is the number of conversations the shell's own poll last saw waiting,
 * so the badge and the inbox can never disagree.
 */
export function InboxButton() {
  const navigate = useNavigate();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const waiting = useInbox((state) => state.entries.length);
  const target = inboxPath(pathname);
  const active = pathname === target;

  return (
    <SidebarButton
      aria-current={active ? "page" : undefined}
      data-active={active || undefined}
      className="data-[active]:bg-muted"
      onClick={() => void navigate({ to: target })}
    >
      <Inbox className={active ? "text-icon-amber" : undefined} />
      <span>Inbox</span>
      {waiting > 0 && (
        <Badge
          variant="outline"
          className="-my-0.5 ml-auto"
          aria-label={`${waiting} waiting on you`}
        >
          {waiting}
        </Badge>
      )}
    </SidebarButton>
  );
}
