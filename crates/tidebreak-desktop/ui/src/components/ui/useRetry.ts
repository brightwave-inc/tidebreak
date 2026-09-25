import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Runs a retry and says whether it is still running, for a Retry whose
 * failure stays on screen until the retry answers.
 *
 * Pass `pending` to `NoticeRetryButton` so the button waits for the answer
 * instead of sending the request again. A retry that clears its own failure
 * as it starts (the notice unmounts and a loading state takes its place)
 * does not need this.
 */
export function useRetry(run: () => unknown): {
  retry: () => void;
  pending: boolean;
} {
  const [pending, setPending] = useState(false);
  const runRef = useRef(run);
  runRef.current = run;
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const retry = useCallback(() => {
    setPending(true);
    void Promise.resolve()
      .then(() => runRef.current())
      // The caller's own state carries the failure; this only tracks whether
      // an attempt is in flight.
      .catch(() => undefined)
      .finally(() => {
        if (mounted.current) setPending(false);
      });
  }, []);

  return { retry, pending };
}
