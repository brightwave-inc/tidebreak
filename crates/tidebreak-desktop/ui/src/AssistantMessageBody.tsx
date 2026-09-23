import { MessageMarkdown } from "./MessageMarkdown";
import { useStreamingTypewriter } from "./useStreamingTypewriter";

/**
 * Assistant prose driven by the typewriter: while the bubble is the live
 * streaming turn its text is typed in, and a settled or rehydrated message
 * renders at once. {@link MessageMarkdown} re-parses only the trailing block
 * as the text grows, and draws a still-open code fence as plain text.
 */
export function AssistantMessageBody({
  text,
  streaming,
  whole = false,
  containerRef,
}: {
  text: string;
  streaming: boolean;
  /**
   * Parse settled text in one pass. For a transcript that opens many settled
   * answers at once; see {@link MessageMarkdown}.
   */
  whole?: boolean;
  containerRef?: React.Ref<HTMLDivElement>;
}) {
  const displayed = useStreamingTypewriter(text, streaming);
  return (
    <MessageMarkdown
      containerRef={containerRef}
      streaming={streaming}
      whole={whole}
    >
      {streaming ? displayed : text}
    </MessageMarkdown>
  );
}
