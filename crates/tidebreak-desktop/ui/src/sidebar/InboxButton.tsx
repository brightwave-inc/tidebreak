import { useNavigate, useRouterState } from "@tanstack/react-router";
import { Inbox } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { useInbox } from "@/Inbox";
import { SidebarButton } from "./primitives";

/**
 * The way to everything that is waiting, from anywhere.
 *
 * It sits on both rails because being blocked is not scoped to a conversation:
 * the reader is most likely to be inside one chat when another one parks. The
 * count is the number of parked items the shell's own poll last saw, so the
 * badge and the inbox can never disagree.
 */
export function InboxButton() {
  const navigate = useNavigate();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const waiting = useInbox((state) => state.items.length);
  const active = pathname === "/inbox";

  return (
    <SidebarButton
      aria-current={active ? "page" : undefined}
      data-active={active || undefined}
      className="data-[active]:bg-muted"
      onClick={() => void navigate({ to: "/inbox" })}
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
