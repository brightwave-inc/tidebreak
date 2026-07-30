import { AlignLeft, Download, FileText } from "lucide-react";

import { PanelBreadcrumb } from "@/components/PanelHeader";
import { Button } from "@/components/ui/button";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { WithTooltip } from "@/components/ui/tooltip";
import type { DocumentView } from "@/components/document/document-details";

/**
 * The panel's header, in the two slots {@link PanelFrame} exposes: the trail
 * back to the list on the left, the view switch and actions on the right.
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
  view,
  onViewChange,
  showOriginalView = true,
  canDownload,
  downloading,
  onDownload,
}: {
  view: DocumentView;
  onViewChange: (view: DocumentView) => void;
  /** Hidden for a format no viewer can draw; its panel is the extracted text. */
  showOriginalView?: boolean;
  canDownload?: boolean;
  downloading?: boolean;
  onDownload: () => void;
}) {
  return (
    <div className="flex items-center gap-2">
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
      {showOriginalView && (
        <Tabs
          value={view}
          onValueChange={(value) => onViewChange(value as DocumentView)}
        >
          <TabsList className="rounded-lg bg-muted p-1">
            <WithTooltip label="Extracted text">
              <TabsTrigger value="extracted_text" className="h-7 w-7 px-0">
                <AlignLeft className="size-4" />
                <span className="sr-only">Extracted text</span>
              </TabsTrigger>
            </WithTooltip>
            <WithTooltip label="Original document">
              <TabsTrigger value="original_doc" className="h-7 w-7 px-0">
                <FileText className="size-4" />
                <span className="sr-only">Original document</span>
              </TabsTrigger>
            </WithTooltip>
          </TabsList>
        </Tabs>
      )}
    </div>
  );
}
