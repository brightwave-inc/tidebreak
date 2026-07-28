import { Button } from "@/components/ui/button";

/**
 * The strip above the grid. Column widths stored in a workbook are frequently
 * narrower than the text they hold, so autofit is the one control worth having
 * within reach of a viewer that otherwise cannot be edited.
 */
export function SpreadsheetShortcutsInfoBar({
  onAutofit,
}: {
  onAutofit?: () => void;
}) {
  if (!onAutofit) return null;

  return (
    <div className="text-muted-foreground flex items-center gap-1 border-b px-2 py-1 text-xs">
      <Button
        variant="ghost"
        size="sm"
        className="text-muted-foreground hover:text-foreground h-6 px-1.5 text-[11px]"
        onClick={onAutofit}
      >
        Autofit all columns
      </Button>
    </div>
  );
}
