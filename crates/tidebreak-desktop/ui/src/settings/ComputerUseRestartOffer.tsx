import { RotateCcw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/notice";

/**
 * The offer to restart after Screen Recording was turned on.
 *
 * macOS applies a Screen Recording grant only to a process started after the
 * grant, so a Tidebreak that asked for it during this run still cannot take
 * screenshots until it restarts. The offer does the restart for the person;
 * working agents get the quit prompt first.
 */
export function ComputerUseRestartOffer({
  restarting,
  disabled = false,
  onRestart,
}: {
  restarting: boolean;
  disabled?: boolean;
  onRestart: () => void;
}) {
  return (
    <Notice
      tone="info"
      action={
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={restarting || disabled}
          onClick={onRestart}
        >
          <RotateCcw aria-hidden="true" />
          {restarting ? "Restarting…" : "Restart Tidebreak"}
        </Button>
      }
    >
      macOS applies Screen Recording after Tidebreak restarts. Once you turn it
      on in System Settings, restart to finish.
    </Notice>
  );
}
