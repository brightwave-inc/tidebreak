import { useCallback, useLayoutEffect, useRef } from "react";

/**
 * A function whose identity never changes and that always calls the latest
 * `callback`.
 *
 * For event handlers handed to memoized children: hooks that return a fresh
 * closure on every render would otherwise defeat the child's memo for nothing.
 * Never call the result during render — it reads the callback committed last.
 */
export function useStableCallback<Args extends unknown[], Result>(
  callback: (...args: Args) => Result,
): (...args: Args) => Result {
  const latest = useRef(callback);
  useLayoutEffect(() => {
    latest.current = callback;
  });
  return useCallback((...args: Args) => latest.current(...args), []);
}
