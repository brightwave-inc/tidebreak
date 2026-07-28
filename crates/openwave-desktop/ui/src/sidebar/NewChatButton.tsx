import { SquarePen } from "lucide-react";

import { Button } from "@/components/ui/button";
import { SidebarButton, useSidebarWidth } from "./primitives";

/**
 * Starting a chat is the rail's primary action, so an expanded rail gives it a
 * prominent full-width button rather than another nav row. A compact rail has
 * no room for that, so it falls back to the icon-only row with its label in a
 * tooltip — the same treatment every other compact control gets.
 */
export function NewChatButton({
  onClick,
  disabled,
  creating,
}: {
  onClick: () => void;
  disabled: boolean;
  creating: boolean;
}) {
  const isCompact = useSidebarWidth() === "compact";
  const label = creating ? "Starting…" : "New chat";

  if (isCompact) {
    return (
      <SidebarButton aria-label={label} onClick={onClick} disabled={disabled}>
        <SquarePen />
        <span>{label}</span>
      </SidebarButton>
    );
  }

  return (
    <Button
      variant="outline"
      size="sm"
      className="w-full justify-start gap-2"
      onClick={onClick}
      disabled={disabled}
    >
      <SquarePen />
      <span>{label}</span>
    </Button>
  );
}
