import { FolderGit2, MessageSquare } from "lucide-react";
import { useEffect } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";

import { storeAppMode } from "@/appMode";
import { SegmentedControl } from "@/components/ui/segmented";
import { isCodeRoute } from "./routes";

type Mode = "chat" | "code";

/**
 * Work / Code switch used at the top of both rails.
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

  useEffect(() => {
    storeAppMode(value === "code" ? "code" : "work");
  }, [value]);

  return (
    <div className="px-2">
      <SegmentedControl
        aria-label="App mode"
        value={value}
        onValueChange={(next) => {
          if (next === value) return;
          storeAppMode(next === "code" ? "code" : "work");
          void navigate({ to: next === "code" ? "/code" : "/" });
        }}
        options={[
          {
            value: "chat",
            label: "Work",
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
