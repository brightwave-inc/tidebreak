import type { PendingFolderAccessRequest } from "./api";
import type { FolderAccessDecision } from "./host";

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
    <section
      className="folder-consent"
      aria-labelledby={`folder-${request.callId}`}
      aria-busy={working}
    >
      <div className="folder-consent-heading">
        <div>
          <h2 id={`folder-${request.callId}`}>Folder access requested</h2>
          <span className="status">read access</span>
        </div>
      </div>
      <p className="folder-consent-reason">{request.reason}</p>
      {hint && (
        <p className="folder-consent-hint">
          Suggested starting location: <strong>{hint}</strong>
        </p>
      )}
      {working ? (
        <p className="folder-consent-note" role="status" aria-live="polite">
          Resolving the folder request…
        </p>
      ) : request.claimedByDesktop ? (
        <p className="folder-consent-note" role="status" aria-live="polite">
          This request is already being handled by the native desktop.
        </p>
      ) : !nativeHost ? (
        <div>
          <p className="folder-consent-note">
            Folder consent is unavailable in browser-only mode. This desktop
            cannot resolve a chat owned by the headless server.
          </p>
          <div className="folder-consent-actions">
            <button type="button" className="btn" onClick={onCancel}>
              Cancel turn
            </button>
          </div>
        </div>
      ) : nativeBusy ? (
        <p className="folder-consent-note" role="status" aria-live="polite">
          Finish the current folder request first.
        </p>
      ) : (
        <div className="folder-consent-actions">
          <button
            type="button"
            className="btn btn-primary"
            disabled={!actionable}
            onClick={() => onDecision("allow")}
          >
            Allow
          </button>
          <button
            type="button"
            className="btn"
            disabled={!actionable}
            onClick={() => onDecision("decline")}
          >
            Decline
          </button>
        </div>
      )}
      {error && (
        <p className="folder-consent-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
