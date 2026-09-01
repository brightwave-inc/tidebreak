import type { ApiClient } from "../../api/client";
import type { LayoutState } from "@/panel/panelTypes";
import {
  adoptCodeTerminalId,
  type CodeEditorRegion,
  codeTerminalIds,
  findCodeTerminalTab,
  focusConversation,
  focusedEditorPosition,
  focusEditorTab,
  openCodeEditor,
  removedCodeTerminalIds,
} from "../codeChrome";
import { friendlyErrorMessage } from "@/lib/utils";
import { MAX_WORKSPACE_TERMINALS, nameTerminals } from "./layout";
import { toast } from "sonner";
import { useCallback, useEffect, useRef, useState } from "react";
import { useCodeUiStore } from "../CodeUiStore";

/**
 * The shell tabs one workspace has open: their labels, the shell behind
 * each, and the chord that jumps to one and back.
 *
 * The server names a shell, so its tab cannot exist before the create call
 * answers — unlike a browser, whose id the page mints itself. Closing a tab
 * is the only thing that ends a shell: a build running in one has to survive
 * a click on another workspace, and the tab's address names it, so coming
 * back re-attaches to the same process with its output intact.
 */
export function useTerminalTabs({
  workspaceId,
  client,
  layout,
  setLayout,
}: {
  workspaceId: string;
  client: Pick<ApiClient, "createCodeTerminal" | "deleteCodeTerminal">;
  layout: LayoutState;
  setLayout: (next: LayoutState) => void;
}) {
  const layoutRef = useRef(layout);
  layoutRef.current = layout;
  const previousLayoutRef = useRef(layout);
  const closedTerminalIdsRef = useRef(new Set<string>());
  /** Where focus sat before the terminal chord jumped away from it. */
  const beforeTerminalRef = useRef<{
    region: CodeEditorRegion;
    index: number;
  } | null>(null);
  const [terminalLabels, setTerminalLabels] = useState<Record<string, string>>(
    () => nameTerminals({}, codeTerminalIds(layout)),
  );
  const terminalPending = useCodeUiStore((state) => state.terminalPending);

  /**
   * End the shells whose tabs have gone.
   *
   * A workspace may only hold so many at once, so a closed tab has to give
   * its shell back rather than leave it running with nothing pointing at it.
   */
  const closeTerminalPanels = useCallback(
    (terminalIds: readonly string[]) => {
      for (const terminalId of terminalIds) {
        if (closedTerminalIdsRef.current.has(terminalId)) continue;
        closedTerminalIdsRef.current.add(terminalId);
        void client.deleteCodeTerminal(workspaceId, terminalId).catch(() => {
          // A shell that is already gone is the outcome we wanted anyway.
        });
      }
    },
    [client, workspaceId],
  );

  useEffect(() => {
    closeTerminalPanels(
      removedCodeTerminalIds(previousLayoutRef.current, layout),
    );
    previousLayoutRef.current = layout;
    setTerminalLabels((current) =>
      nameTerminals(current, codeTerminalIds(layout)),
    );
  }, [closeTerminalPanels, layout]);

  /**
   * Start a shell and give it a tab.
   *
   * The layout is read after the call answers, so a tab the reader opened
   * meanwhile is not lost to a stale snapshot.
   */
  async function openTerminal(preferredRegion?: CodeEditorRegion) {
    try {
      const snap = await client.createCodeTerminal(workspaceId);
      setTerminalLabels((current) => nameTerminals(current, [snap.id]));
      setLayout(
        openCodeEditor(
          layoutRef.current,
          { type: "terminal", terminalId: snap.id },
          preferredRegion,
        ),
      );
    } catch (error) {
      toast.error(friendlyErrorMessage(error, "Could not open a terminal"));
    }
  }

  /**
   * Jump to the terminal and back again.
   *
   * The chord used to show and hide a drawer. A terminal is a tab now, so the
   * same press moves focus there and the next one returns it where it was —
   * the flick there and back a drawer gave, without a second kind of surface.
   */
  function toggleTerminal() {
    const found = findCodeTerminalTab(layoutRef.current);
    if (!found) {
      beforeTerminalRef.current = focusedEditorPosition(layoutRef.current);
      void openTerminal("primary");
      return;
    }
    const focused = focusedEditorPosition(layoutRef.current);
    const onTerminal =
      focused?.region === found.region && focused.index === found.index;
    if (!onTerminal) {
      beforeTerminalRef.current = focused;
      setLayout(focusEditorTab(layoutRef.current, found.index, found.region));
      return;
    }
    const back = beforeTerminalRef.current;
    beforeTerminalRef.current = null;
    setLayout(
      back
        ? focusEditorTab(layoutRef.current, back.index, back.region)
        : focusConversation(layoutRef.current),
    );
  }

  /** A pane attached to a different shell than its tab named. */
  function adoptTerminal(previousId: string | undefined, terminalId: string) {
    setLayout(adoptCodeTerminalId(layoutRef.current, previousId, terminalId));
  }

  // The chord and the rail command both raise the ask above the route, because
  // starting a shell is a server call neither of them can make.
  useEffect(() => {
    if (!terminalPending) return;
    if (!useCodeUiStore.getState().takeTerminal()) return;
    toggleTerminal();
    // The flag is the trigger; the rest is state read when it arrives.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalPending]);

  const openTerminalCount = codeTerminalIds(layout).length;
  const hasTerminal = findCodeTerminalTab(layout) !== null;
  const canNewTerminal = openTerminalCount < MAX_WORKSPACE_TERMINALS;

  return {
    terminalLabels,
    openTerminal,
    toggleTerminal,
    adoptTerminal,
    hasTerminal,
    canNewTerminal,
  };
}
