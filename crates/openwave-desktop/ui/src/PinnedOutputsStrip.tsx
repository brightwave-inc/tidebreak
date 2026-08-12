import { ChevronDown, ChevronUp } from "lucide-react";
import { create } from "zustand";

import { DocumentIcon } from "./components/document-table/DocumentIcon";
import type { DeliverableSummary } from "./deliverables";
import { cn } from "@/lib/utils";

/** Past this many the rest collapse into a count rather than wrapping on. */
const MAX_VISIBLE_OUTPUTS = 4;

const COLLAPSED_PREFIX = "outputs_strip_collapsed_";

function readStoredCollapsed(): Record<string, boolean> {
  const collapsed: Record<string, boolean> = {};
  try {
    const storage = window.sessionStorage;
    for (let index = 0; index < storage.length; index += 1) {
      const name = storage.key(index);
      if (!name?.startsWith(COLLAPSED_PREFIX)) continue;
      if (storage.getItem(name) === "true") {
        collapsed[name.slice(COLLAPSED_PREFIX.length)] = true;
      }
    }
  } catch {
    // A strip with no memory still works; an unreadable store is not fatal.
  }
  return collapsed;
}

function writeStoredCollapsed(chatId: string, collapsed: boolean): void {
  try {
    if (collapsed) {
      window.sessionStorage.setItem(`${COLLAPSED_PREFIX}${chatId}`, "true");
    } else {
      window.sessionStorage.removeItem(`${COLLAPSED_PREFIX}${chatId}`);
    }
  } catch {
    // Persisting the preference is best-effort.
  }
}

/**
 * Which conversations the reader has folded the strip away in.
 *
 * The chat route is remounted per conversation, so this cannot live in the
 * component: folding the strip away and glancing at another chat would unfold
 * it again on the way back. Session storage rather than local — collapsing is a
 * "not now", not a setting, and a fresh launch should show the outputs again.
 */
type OutputsStripStore = {
  collapsed: Record<string, boolean>;
  setCollapsed: (chatId: string, collapsed: boolean) => void;
};

export function createOutputsStripStore() {
  return create<OutputsStripStore>()((set) => ({
    collapsed: readStoredCollapsed(),
    setCollapsed: (chatId, collapsed) => {
      writeStoredCollapsed(chatId, collapsed);
      set((state) => {
        const next = { ...state.collapsed };
        if (collapsed) next[chatId] = true;
        else delete next[chatId];
        return { collapsed: next };
      });
    },
  }));
}

export const useOutputsStripStore = createOutputsStripStore();

export type PinnedOutputsStripProps = {
  chatId: string;
  outputs: readonly DeliverableSummary[];
  /** True while a panel is open beside the conversation. */
  panelOpen: boolean;
  /** Open one output in the panel region. */
  onOpenOutput: (outputId: string) => void;
  /** Bring the whole outputs list forward. */
  onOpenOutputs: () => void;
};

/**
 * What this conversation has produced, pinned above the transcript.
 *
 * The header chip counts outputs but is easy to read past, and the files are
 * the point of most conversations that make any — so they get named where the
 * reader is already looking, from the first one onwards.
 *
 * It yields to the panel region: once a panel is open the outputs are either
 * already on screen or one tab away, and a second copy of them above the
 * transcript is just chrome. Collapsing is the reader's own call and outlives
 * the panel, so it is remembered per conversation.
 */
export function PinnedOutputsStrip({
  chatId,
  outputs,
  panelOpen,
  onOpenOutput,
  onOpenOutputs,
}: PinnedOutputsStripProps) {
  const collapsed = useOutputsStripStore((state) => state.collapsed[chatId] ?? false);
  const setCollapsed = useOutputsStripStore((state) => state.setCollapsed);

  if (panelOpen || outputs.length === 0) return null;

  // Newest first: the file the last turn wrote is the one being looked for.
  const ordered = [...outputs].sort(
    (a, b) => Date.parse(b.updatedAt) - Date.parse(a.updatedAt),
  );
  const visible = ordered.slice(0, MAX_VISIBLE_OUTPUTS);
  const hidden = ordered.length - visible.length;
  const label = outputs.length === 1 ? "1 output" : `${outputs.length} outputs`;

  return (
    <section
      className="mt-1 flex shrink-0 flex-wrap items-center gap-x-2 gap-y-1 px-4 py-1"
      aria-label="Outputs"
    >
      <button
        type="button"
        className="cursor-pointer text-xs text-muted-foreground transition-colors hover:text-foreground"
        onClick={onOpenOutputs}
      >
        {label}
      </button>
      {!collapsed &&
        visible.map((output) => (
          <OutputChip
            key={output.outputId}
            output={output}
            onOpen={() => onOpenOutput(output.outputId)}
          />
        ))}
      {!collapsed && hidden > 0 && (
        <button
          type="button"
          className="cursor-pointer text-xs text-muted-foreground transition-colors hover:text-foreground"
          onClick={onOpenOutputs}
        >
          {`+${hidden} more`}
        </button>
      )}
      <button
        type="button"
        className="ml-auto inline-flex size-5 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        aria-label={collapsed ? "Show outputs" : "Hide outputs"}
        aria-expanded={!collapsed}
        onClick={() => setCollapsed(chatId, !collapsed)}
      >
        {collapsed ? (
          <ChevronDown className="size-3.5" aria-hidden="true" />
        ) : (
          <ChevronUp className="size-3.5" aria-hidden="true" />
        )}
      </button>
    </section>
  );
}

function OutputChip({
  output,
  onOpen,
}: {
  output: DeliverableSummary;
  onOpen: () => void;
}) {
  return (
    <button
      type="button"
      className={cn(
        "bg-background inline-flex max-w-56 min-w-0 cursor-pointer items-center gap-1.5",
        "rounded-md border px-2 py-0.5 text-xs transition-colors hover:bg-accent",
      )}
      onClick={onOpen}
      aria-label={`Open output ${output.filename}`}
    >
      <DocumentIcon
        mediaType={output.mediaType}
        className="size-3.5"
        aria-hidden="true"
      />
      <span className="min-w-0 truncate">{output.filename}</span>
    </button>
  );
}
