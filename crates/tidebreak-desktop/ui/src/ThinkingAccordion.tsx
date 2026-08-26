import { useEffect, useRef, useState } from "react";
import { ChevronDown, Lightbulb } from "lucide-react";

import { LiveLabel } from "./LiveLabel";
import { MessageMarkdown } from "./MessageMarkdown";
import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { cn } from "@/lib/utils";

/**
 * The model's reasoning for one step, behind a disclosure.
 *
 * Open while the reasoning is still arriving, because that is when it is the
 * only thing to read; closed once the answer starts, because by then it is
 * background. A settled transcript therefore shows one quiet "Thought" line per
 * step rather than paragraphs of preamble above every answer.
 *
 * Auto-collapse defers to the reader: once the disclosure has been clicked, it
 * stays where it was put. Reasoning that snaps shut while it is being read is
 * worse than reasoning that stays open.
 */
export function ThinkingAccordion({
  text,
  streaming,
  expandWhileStreaming = true,
}: {
  text: string;
  /** Reasoning is still arriving and no answer has started. */
  streaming: boolean;
  /**
   * Whether live reasoning opens itself.
   *
   * A surface that pauses to think several times in one turn — code mode does,
   * between every pair of tool calls — would otherwise stack open blocks until
   * they are the whole viewport. It passes false: the line still says
   * "Thinking" and still shimmers, so the work is legible, but the text is one
   * click away rather than in the way.
   */
  expandWhileStreaming?: boolean;
}) {
  const [expanded, setExpanded] = useState(streaming && expandWhileStreaming);
  const toggledByReader = useRef(false);

  useEffect(() => {
    if (streaming || toggledByReader.current) return;
    setExpanded(false);
  }, [streaming]);

  if (!text.trim()) return null;

  return (
    <Collapsible
      open={expanded}
      onOpenChange={(next) => {
        toggledByReader.current = true;
        setExpanded(next);
      }}
      className="w-full self-start"
    >
      <CollapsibleTrigger asChild>
        <Button
          type="button"
          variant="link"
          className="text-muted-foreground h-auto px-0 py-1"
        >
          <Lightbulb className="size-4" />
          <LiveLabel live={streaming}>
            {streaming ? "Thinking" : "Thought"}
          </LiveLabel>
          <ChevronDown
            className={cn(
              "size-4 transition-transform",
              !expanded && "-rotate-90",
            )}
          />
        </Button>
      </CollapsibleTrigger>
      <CollapsibleContent className="text-muted-foreground border-border ml-1.5 border-l-2 py-1 pl-4 text-sm">
        <MessageMarkdown>{text}</MessageMarkdown>
      </CollapsibleContent>
    </Collapsible>
  );
}
