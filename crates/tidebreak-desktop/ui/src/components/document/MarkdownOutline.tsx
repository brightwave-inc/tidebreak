import { ListTreeIcon } from "lucide-react";
import { useState } from "react";

import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { MarkdownHeading } from "@/markdownHeadings";

/**
 * Nesting is drawn with indentation rather than a tree, because the outline of a
 * document that skips from h1 to h4 should still read as a flat ordered list of
 * places to go. Levels 5 and 6 share an indent: past four the indent costs more
 * width than the extra depth conveys.
 */
const LEVEL_INDENT: Record<number, string> = {
  1: "pl-2",
  2: "pl-5",
  3: "pl-8",
  4: "pl-11",
  5: "pl-14",
  6: "pl-14",
};

export function MarkdownOutline({
  headings,
  onNavigate,
  triggerClassName,
}: {
  headings: MarkdownHeading[];
  /** Reveal the heading with this id. */
  onNavigate: (id: string) => void;
  triggerClassName?: string;
}) {
  const [open, setOpen] = useState(false);

  if (headings.length === 0) return null;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <WithTooltip label="Document outline">
        <PopoverTrigger asChild>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Document outline"
            className={triggerClassName}
          >
            <ListTreeIcon className="size-4" />
          </Button>
        </PopoverTrigger>
      </WithTooltip>
      <PopoverContent
        align="end"
        sideOffset={4}
        className="w-72 p-1"
        // Returning focus to the trigger would scroll the document back to
        // where it was, undoing the jump the reader just asked for.
        onCloseAutoFocus={(event) => event.preventDefault()}
      >
        <div className="max-h-80 space-y-0.5 overflow-y-auto">
          {headings.map((heading, index) => (
            <button
              key={`${heading.id}-${index}`}
              type="button"
              className={cn(
                "flex w-full cursor-pointer items-center rounded-md py-1.5 pr-2 text-left text-sm transition-colors hover:bg-muted",
                LEVEL_INDENT[heading.level],
              )}
              onClick={() => {
                onNavigate(heading.id);
                setOpen(false);
              }}
            >
              <span className="truncate">{heading.text}</span>
            </button>
          ))}
        </div>
      </PopoverContent>
    </Popover>
  );
}
