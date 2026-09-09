import type { ReactNode } from "react";
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable";

/** Keep the primary editor mounted when an agent opens or closes a preview. */
export function CodeEditorGroups({
  primary,
  secondary,
}: {
  primary: ReactNode;
  secondary?: ReactNode;
}) {
  const split = secondary != null;
  return (
    <ResizablePanelGroup orientation="horizontal" className="h-full min-h-0">
      <ResizablePanel
        id="editor-primary"
        defaultSize={split ? "55" : "100"}
        minSize="25"
        className="min-w-0"
      >
        <div
          data-code-editor-region="primary"
          className="flex h-full min-h-0 flex-col"
        >
          {primary}
        </div>
      </ResizablePanel>
      {split && <ResizableHandle />}
      {split && (
        <ResizablePanel
          id="editor-split"
          defaultSize="45"
          minSize="25"
          className="min-w-0"
        >
          <div
            data-code-editor-region="secondary"
            className="flex h-full min-h-0 flex-col"
          >
            {secondary}
          </div>
        </ResizablePanel>
      )}
    </ResizablePanelGroup>
  );
}
