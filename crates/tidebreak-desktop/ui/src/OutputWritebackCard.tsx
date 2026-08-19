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
  const replacing = request.mode === "replace";
  const title = replacing
    ? "Replace an existing file?"
    : "Write a file to a connected folder?";
  const subtitle = replacing
    ? "The agent wants to replace a file in a folder connected to this work. The native desktop will verify the connected folder, destination, and current output revision before writing."
    : "The agent wants to write one of this work's outputs into a folder connected to this work. The native desktop will verify the connected folder, destination, and current output revision before writing.";
  const allowLabel = replacing ? "Allow replacement" : "Allow write";
  const unavailable = replacing
    ? "File replacement is unavailable in browser-only mode."
    : "Writing to connected folders is unavailable in browser-only mode.";

  return (
    <AttentionCard
      title={title}
      titleId={`output-writeback-${request.callId}`}
      subtitle={subtitle}
      busy={working}
      error={error}
    >
      {working ? (
        <p
          className="text-muted-foreground text-sm"
          role="status"
          aria-live="polite"
        >
          {replacing
            ? "Resolving the replacement request…"
            : "Resolving the write request…"}
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
          <p className="text-muted-foreground text-sm">{unavailable}</p>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" size="sm" onClick={onCancel}>
              Cancel turn
            </Button>
          </div>
        </>
      ) : (
        <div className="flex flex-wrap gap-2">
          <Button
            size="sm"
            disabled={!actionable}
            onClick={() => onDecision("allow")}
          >
            {allowLabel}
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
