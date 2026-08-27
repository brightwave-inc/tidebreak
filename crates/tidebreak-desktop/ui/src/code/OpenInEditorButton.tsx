import { ExternalLink } from "lucide-react";

import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { openInEditorLabel, useEditorPreference } from "./editorPreference";

/**
 * The one "hand this file to my editor" control, shared by the file viewer and
 * the diff panel.
 *
 * Both panels sit under the same center tab strip and share one header shape,
 * so the action has to look and read identically in the two of them. It sits
 * beside the diff panel's "Open file" and wears the same quiet chip treatment:
 * a recognition glyph at text size, no container, no colour of its own.
 */
export function OpenInEditorButton({ onClick }: { onClick: () => void }) {
  const label = openInEditorLabel(useEditorPreference().editor);
  return (
    <button
      type="button"
      className={cn(
        "text-muted-foreground hover:bg-muted hover:text-foreground flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-1 text-xs",
        FOCUS_RING_TIGHT,
        HOVER_TINT,
      )}
      onClick={onClick}
    >
      <ExternalLink className="size-3" aria-hidden />
      {label}
    </button>
  );
}
