import { MessageMarkdown } from "./MessageMarkdown";
import { useStreamingTypewriter } from "./useStreamingTypewriter";

/**
 * Assistant prose driven by the typewriter: while the bubble is the live
 * streaming turn its text is typed in, and a settled or rehydrated message
 * renders at once. Block-level memoization inside {@link MessageMarkdown} keeps
 * each tick's re-parse confined to the trailing block.
 */
export function AssistantMessageBody({
  text,
  streaming,
  containerRef,
}: {
  text: string;
  streaming: boolean;
  containerRef?: React.Ref<HTMLDivElement>;
}) {
  const displayed = useStreamingTypewriter(text, streaming);
  return (
    <MessageMarkdown containerRef={containerRef}>{displayed}</MessageMarkdown>
  );
}
