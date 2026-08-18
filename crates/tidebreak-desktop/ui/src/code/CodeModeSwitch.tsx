import { FolderGit2, MessageSquare } from "lucide-react";
import { useNavigate, useRouterState } from "@tanstack/react-router";

import { SegmentedControl } from "@/components/ui/segmented";
import { useSidebarWidth } from "@/sidebar/primitives";
import { isCodeRoute } from "./routes";

type Mode = "chat" | "code";

/**
 * Chat / Code switch used at the top of both rails.
 *
 * One primitive, two hosts: the code rail always shows it, and the chat rail
 * shows it only when the code-mode flag is on.
 */
export function CodeModeSwitch() {
  const navigate = useNavigate();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const isCompact = useSidebarWidth() === "compact";
  const value: Mode = isCodeRoute(pathname) ? "code" : "chat";

  return (
    <div className="px-2">
      <SegmentedControl
        aria-label="App mode"
        value={value}
        compact={isCompact}
        onValueChange={(next) => {
          if (next === value) return;
          void navigate({ to: next === "code" ? "/code" : "/" });
        }}
        options={[
          {
            value: "chat",
            label: "Chat",
            icon: <MessageSquare className="size-3.5 shrink-0" aria-hidden />,
          },
          {
            value: "code",
            label: "Code",
            icon: <FolderGit2 className="size-3.5 shrink-0" aria-hidden />,
          },
        ]}
      />
    </div>
  );
}
