import { FolderGit2, MessageSquare } from "lucide-react";
import { useNavigate, useRouterState } from "@tanstack/react-router";

import { SegmentedControl } from "@/components/ui/segmented";
import { isCodeRoute } from "./routes";

type Mode = "chat" | "code";

/**
 * Chat / Code switch used at the top of both rails.
 *
 * One primitive, two hosts: both rails always show it so Code remains a
 * first-class product surface rather than a setting the reader has to find.
 */
export function CodeModeSwitch() {
  const navigate = useNavigate();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const value: Mode = isCodeRoute(pathname) ? "code" : "chat";

  return (
    <div className="px-2">
      <SegmentedControl
        aria-label="App mode"
        value={value}
        onValueChange={(next) => {
          if (next === value) return;
          void navigate({ to: next === "code" ? "/code" : "/" });
        }}
        options={[
          {
            value: "chat",
            label: "Chat",
            icon: (
              <MessageSquare
                className={`${value === "chat" ? "text-icon-blue" : ""} size-3.5 shrink-0`}
                aria-hidden
              />
            ),
          },
          {
            value: "code",
            label: "Code",
            icon: (
              <FolderGit2
                className={`${value === "code" ? "text-icon-violet" : ""} size-3.5 shrink-0`}
                aria-hidden
              />
            ),
          },
        ]}
      />
    </div>
  );
}
