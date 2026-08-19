/**
 * Shared frame for compact, actionable things a work/chat turn produced.
 *
 * Apps and ordinary file outputs carry different content, but they should
 * occupy the same responsive footprint in the transcript. Rich inline
 * surfaces such as charts and MCP views keep their own content-sized frames.
 */
export const TRANSCRIPT_RESULT_CARD_FRAME =
  "bg-background flex w-full max-w-md min-w-0 items-center gap-3 rounded-lg border px-4 py-3 text-left shadow-sm";
