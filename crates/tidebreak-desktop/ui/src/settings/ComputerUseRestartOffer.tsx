import { RotateCcw } from "lucide-react";

import { Button } from "@/components/ui/button";

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
    <div
      role="status"
      className="notice-surface notice-info flex flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border px-3 py-2 text-sm"
    >
      <p className="min-w-0 flex-1 basis-56">
        macOS applies Screen Recording after Tidebreak restarts. Once you turn
        it on in System Settings, restart to finish.
      </p>
      <Button
        type="button"
        size="xs"
        variant="outline"
        disabled={restarting || disabled}
        onClick={onRestart}
      >
        <RotateCcw aria-hidden="true" />
        {restarting ? "Restarting…" : "Restart Tidebreak"}
      </Button>
    </div>
  );
}
