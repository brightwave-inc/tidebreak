import { useEffect, useRef, useState } from "react";
import { ChevronDown, Lightbulb } from "lucide-react";

import { MessageMarkdown } from "./MessageMarkdown";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

let accordionSeq = 0;

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
}: {
  text: string;
  /** Reasoning is still arriving and no answer has started. */
  streaming: boolean;
}) {
  const [expanded, setExpanded] = useState(streaming);
  const toggledByReader = useRef(false);
  const contentId = useRef(`thinking-${(accordionSeq += 1)}`).current;

  useEffect(() => {
    if (streaming || toggledByReader.current) return;
    setExpanded(false);
  }, [streaming]);

  if (!text.trim()) return null;

  return (
    <div className="w-full self-start">
      <Button
        type="button"
        variant="link"
        className="text-muted-foreground h-auto px-0 py-1"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={() => {
          toggledByReader.current = true;
          setExpanded((current) => !current);
        }}
      >
        <Lightbulb className="size-4" />
        <span className={cn(streaming && "animate-pulse")}>
          {streaming ? "Thinking" : "Thought"}
        </span>
        <ChevronDown
          className={cn(
            "size-4 transition-transform",
            !expanded && "-rotate-90",
          )}
        />
      </Button>
      {expanded && (
        <div
          id={contentId}
          className="text-muted-foreground border-border ml-1.5 border-l-2 py-1 pl-4 text-sm"
        >
          <MessageMarkdown>{text}</MessageMarkdown>
        </div>
      )}
    </div>
  );
}
