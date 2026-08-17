import { useNavigate } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { FlaskConical } from "lucide-react";

import { Button } from "@/components/ui/button";
import { useExperimentalFlags } from "@/experimental";
import { RouteFrame } from "@/RouteFrame";
import { AppSidebar } from "@/sidebar/AppSidebar";

/**
 * Keeps the code-mode pages behind the experimental opt-in.
 *
 * The routes still resolve — a deep link or a stale history entry lands on
 * this explanation rather than a dead end — but nothing code-mode renders
 * until the reader has flipped the switch. Until the flags load the gate
 * shows nothing rather than flashing a surface it may be about to hide.
 */
export function CodeModeGate({ children }: { children: ReactNode }) {
  const navigate = useNavigate();
  const loaded = useExperimentalFlags((state) => state.loaded);
  const enabled = useExperimentalFlags((state) => state.codeModeEnabled);
  if (enabled) return <>{children}</>;
  return (
    <RouteFrame sidebar={<AppSidebar />}>
      <div className="content-container flex w-full flex-1 items-center justify-center">
        {loaded && (
          <div className="flex max-w-sm flex-col items-center gap-3 text-center">
            <FlaskConical className="text-muted-foreground size-6" />
            <h1 className="text-lg font-medium">Code mode is experimental</h1>
            <p className="text-muted-foreground text-sm">
              Drive coding agents like Claude Code in isolated workspaces on
              your repositories. Turn it on to try it.
            </p>
            <Button
              type="button"
              onClick={() => {
                const experimentalPath: string = "/settings/experimental";
                void navigate({ to: experimentalPath });
              }}
            >
              Open experimental settings
            </Button>
          </div>
        )}
      </div>
    </RouteFrame>
  );
}
