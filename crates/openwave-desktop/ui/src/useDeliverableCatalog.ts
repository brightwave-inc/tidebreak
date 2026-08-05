import { useEffect, useState } from "react";

import { listDeliverables, type DeliverableSummary } from "./deliverables";
import { useRefreshSignals } from "./RefreshSignals";

/**
 * The chat's outputs, as summaries: enough to count them and to name a tab
 * after one. Fetched on mount and again whenever a refresh signal says the
 * catalog could have moved; errors are swallowed, leaving the last known list,
 * because a stale name in a chip or a tab is better than an error state there.
 */
export function useDeliverableCatalog(chatId: string): DeliverableSummary[] {
  const [deliverables, setDeliverables] = useState<DeliverableSummary[]>([]);
  const outputWritebacks = useRefreshSignals((s) => s.outputWritebacks);

  useEffect(() => {
    let cancelled = false;
    listDeliverables(chatId).then(
      (catalog) => {
        if (!cancelled) setDeliverables(catalog.deliverables);
      },
      () => {
        /* swallow — stale summaries are acceptable */
      },
    );
    return () => {
      cancelled = true;
    };
  }, [chatId, outputWritebacks]);

  return deliverables;
}
