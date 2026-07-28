import {
  importIsActive,
  sortedImportQueue,
  useImportQueueStore,
} from "./ImportQueueStore";

export function ImportQueue() {
  const entries = useImportQueueStore((state) => state.entries);
  const dismissCleanRun = useImportQueueStore((state) => state.dismissCleanRun);
  if (entries.length === 0) return null;

  const sorted = sortedImportQueue(entries);
  const active = sorted.some((entry) => importIsActive(entry.status));
  const failed = sorted.some((entry) => entry.status === "failed");

  return (
    <aside className="import-queue" aria-labelledby="import-queue-title">
      <header>
        <div>
          <h2 id="import-queue-title">
            {active ? "Adding sources" : failed ? "Some sources need attention" : "Sources added"}
          </h2>
          <p>{active ? "OpenWave is preparing your files for this conversation." : null}</p>
        </div>
        {!active && !failed && (
          <button type="button" className="btn" onClick={dismissCleanRun}>
            Dismiss
          </button>
        )}
      </header>
      <ul aria-live="polite">
        {sorted.map((entry) => (
          <li key={entry.importId} className={`import-queue-${entry.status}`}>
            <span className="import-queue-name">{entry.displayName}</span>
            <span className="import-queue-state">
              {importStateCopy(entry.status, entry.message)}
            </span>
          </li>
        ))}
      </ul>
    </aside>
  );
}

function importStateCopy(status: string, message: string | null): string {
  switch (status) {
    case "queued":
      return "Waiting to add";
    case "streaming":
      return "Adding";
    case "imported":
      return "Added";
    case "already_present":
      return "Already added";
    case "failed":
      return message ?? "Could not add";
    default:
      return "Adding";
  }
}
