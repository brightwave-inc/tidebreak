import type { PendingOutputWritebackRequest } from "./api";
import type { OutputWritebackDecision } from "./host";
import { AttentionCard } from "./AttentionCard";
import { Button } from "@/components/ui/button";

export function OutputWritebackCard({
  request,
  nativeHost,
  working,
  error,
  onDecision,
  onCancel,
}: {
  request: PendingOutputWritebackRequest;
  nativeHost: boolean;
  working: boolean;
  error: string | undefined;
  onDecision: (decision: OutputWritebackDecision) => void;
  onCancel: () => void;
}) {
  const actionable = nativeHost && !request.claimedByDesktop && !working;

  return (
    <AttentionCard
      title="Replace an existing file?"
      titleId={`output-writeback-${request.callId}`}
      subtitle="The agent wants to replace a file in a folder connected to this chat. The native desktop will verify the connected folder, destination, and current output revision before writing."
      busy={working}
      error={error}
    >
      {working ? (
        <p
          className="text-muted-foreground text-sm"
          role="status"
          aria-live="polite"
        >
          Resolving the replacement request…
        </p>
      ) : request.claimedByDesktop ? (
        <p
          className="text-muted-foreground text-sm"
          role="status"
          aria-live="polite"
        >
          This request is already being handled by the native desktop.
        </p>
      ) : !nativeHost ? (
        <>
          <p className="text-muted-foreground text-sm">
            File replacement is unavailable in browser-only mode.
          </p>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" size="sm" onClick={onCancel}>
              Cancel turn
            </Button>
          </div>
        </>
      ) : (
        <div className="flex flex-wrap gap-2">
          <Button size="sm" disabled={!actionable} onClick={() => onDecision("allow")}>
            Allow replacement
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={!actionable}
            onClick={() => onDecision("decline")}
          >
            Decline
          </Button>
        </div>
      )}
    </AttentionCard>
  );
}
