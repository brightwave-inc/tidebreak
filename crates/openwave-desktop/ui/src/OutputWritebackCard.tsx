import type { PendingOutputWritebackRequest } from "./api";
import type { OutputWritebackDecision } from "./host";

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
    <section
      className="folder-consent"
      aria-labelledby={`output-writeback-${request.callId}`}
      aria-busy={working}
    >
      <div className="folder-consent-heading">
        <div>
          <h2 id={`output-writeback-${request.callId}`}>
            Replace an existing file?
          </h2>
          <span className="status">sensitive write</span>
        </div>
      </div>
      <p className="folder-consent-reason">
        The agent wants to replace a file in a folder connected to this chat.
        The native desktop will verify the connected folder, destination, and
        current output revision before writing.
      </p>
      {working ? (
        <p className="folder-consent-note" role="status" aria-live="polite">
          Resolving the replacement request…
        </p>
      ) : request.claimedByDesktop ? (
        <p className="folder-consent-note" role="status" aria-live="polite">
          This request is already being handled by the native desktop.
        </p>
      ) : !nativeHost ? (
        <div>
          <p className="folder-consent-note">
            File replacement is unavailable in browser-only mode.
          </p>
          <div className="folder-consent-actions">
            <button type="button" className="btn" onClick={onCancel}>
              Cancel turn
            </button>
          </div>
        </div>
      ) : (
        <div className="folder-consent-actions">
          <button
            type="button"
            className="btn btn-primary"
            disabled={!actionable}
            onClick={() => onDecision("allow")}
          >
            Allow replacement
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
