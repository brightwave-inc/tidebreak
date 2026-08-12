import type { PendingFolderAccessRequest } from "./api";
import type { FolderAccessDecision } from "./host";
import { AttentionCard } from "./AttentionCard";
import { Button } from "@/components/ui/button";

export function FolderAccessCard({
  request,
  nativeHost,
  nativeBusy,
  working,
  error,
  onDecision,
  onCancel,
}: {
  request: PendingFolderAccessRequest;
  nativeHost: boolean;
  nativeBusy: boolean;
  working: boolean;
  error: string | undefined;
  onDecision: (decision: FolderAccessDecision) => void;
  onCancel: () => void;
}) {
  const hint = request.folderHint
    ? request.folderHint[0].toUpperCase() + request.folderHint.slice(1)
    : null;
  const actionable =
    nativeHost && !nativeBusy && !request.claimedByDesktop && !working;

  return (
    <AttentionCard
      title="Folder access requested"
      titleId={`folder-${request.callId}`}
      subtitle={request.reason}
      busy={working}
      error={error}
    >
      {hint && (
        <p className="text-muted-foreground text-sm break-words">
          Suggested starting location: <strong>{hint}</strong>
        </p>
      )}
      {working ? (
        <p
          className="text-muted-foreground text-sm"
          role="status"
          aria-live="polite"
        >
          Resolving the folder request…
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
            Folder consent is unavailable in browser-only mode. This desktop
            cannot resolve a chat owned by the headless server.
          </p>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" size="sm" onClick={onCancel}>
              Cancel turn
            </Button>
          </div>
        </>
      ) : nativeBusy ? (
        <p
          className="text-muted-foreground text-sm"
          role="status"
          aria-live="polite"
        >
          Finish the current folder request first.
        </p>
      ) : (
        <div className="flex flex-wrap gap-2">
          <Button size="sm" disabled={!actionable} onClick={() => onDecision("allow")}>
            Allow
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
