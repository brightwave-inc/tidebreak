import { Plus, Upload } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

type ToolsMenuProps = {
  disabled?: boolean;
  onAttach?: () => Promise<void>;
  attaching?: boolean;
};

export function ToolsMenu({
  disabled,
  onAttach,
  attaching,
}: ToolsMenuProps) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="outline"
          size="icon-8"
          aria-label={attaching ? "Attaching" : "Add"}
          disabled={disabled || attaching}
        >
          <Plus aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" className="w-56">
        {onAttach && (
          <DropdownMenuItem
            disabled={disabled || attaching}
            onSelect={() => void onAttach()}
          >
            <Upload className="size-4" />
            Upload files
          </DropdownMenuItem>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
