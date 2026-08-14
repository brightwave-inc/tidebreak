import { Check, Download, FolderPlus } from "lucide-react";

import { PanelBreadcrumb } from "@/components/PanelHeader";
import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";

/**
 * The panel's header, in the two slots {@link PanelFrame} exposes: the trail
 * back to the list on the left, the document actions on the right.
 */

export function DocumentDetailBreadcrumb({
  documentName,
}: {
  documentName?: string;
}) {
  return (
    <PanelBreadcrumb
      firstPart="Document"
      currentItem={documentName}
    />
  );
}

export function DocumentDetailActions({
  canDownload,
  downloading,
  onDownload,
  canAddToProject,
  sharing,
  shared,
  onAddToProject,
}: {
  canDownload?: boolean;
  downloading?: boolean;
  onDownload: () => void;
  /** Only conversations filed under a project have somewhere to share to. */
  canAddToProject?: boolean;
  sharing?: boolean;
  /** Held for the rest of the panel's life, so the click reads as done. */
  shared?: boolean;
  onAddToProject: () => void;
}) {
  return (
    <div className="flex items-center gap-2">
      {canAddToProject && (
        <WithTooltip
          label={shared ? "In the project" : "Add to project"}
        >
          <Button
            variant="ghost"
            size="icon-sm"
            disabled={sharing || shared}
            onClick={onAddToProject}
          >
            {shared ? (
              <Check className="size-4" />
            ) : (
              <FolderPlus className="size-4" />
            )}
            <span className="sr-only">
              {shared ? "In the project" : "Add to project"}
            </span>
          </Button>
        </WithTooltip>
      )}
      {canDownload && (
        <WithTooltip label="Download">
          <Button
            variant="ghost"
            size="icon-sm"
            disabled={downloading}
            onClick={onDownload}
          >
            <Download className="size-4" />
            <span className="sr-only">Download</span>
          </Button>
        </WithTooltip>
      )}
    </div>
  );
}
