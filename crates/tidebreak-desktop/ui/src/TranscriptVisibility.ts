import { createContext, useContext } from "react";

/**
 * Whether the transcript slot is on screen. Expanding a surface over the
 * transcript leaves it mounted and streaming but without layout, so scrolling
 * it has no effect while it is away — a reader who was following the latest
 * message has to be returned there when it comes back.
 */
const TranscriptVisibilityContext = createContext(true);

export const TranscriptVisibilityProvider =
  TranscriptVisibilityContext.Provider;

export function useTranscriptVisible(): boolean {
  return useContext(TranscriptVisibilityContext);
}
