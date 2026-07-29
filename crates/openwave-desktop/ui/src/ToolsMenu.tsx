import { Check, Plus, Quote, Upload } from "lucide-react";
import type { CitationFormat } from "./api";
import { CITATION_FORMAT_LABELS, CITATION_FORMAT_OPTIONS } from "./CitationFormats";
import { DefaultRow } from "./ModelMenu";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";

type ToolsMenuProps = {
  disabled?: boolean;
  onAttach?: () => Promise<void>;
  attaching?: boolean;
  citationFormat: CitationFormat | null;
  defaultCitationFormat: CitationFormat;
  onCitationFormatChange: (format: CitationFormat | null) => void | Promise<void>;
};

export function ToolsMenu({
  disabled,
  onAttach,
  attaching,
  citationFormat,
  defaultCitationFormat,
  onCitationFormatChange,
}: ToolsMenuProps) {
  const isDefault = citationFormat === null;
  const citationLabel = isDefault
    ? "Default"
    : CITATION_FORMAT_LABELS[citationFormat];

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
          <>
            <DropdownMenuItem
              disabled={disabled || attaching}
              onSelect={() => void onAttach()}
            >
              <Upload className="size-4" />
              Upload files
            </DropdownMenuItem>
            <DropdownMenuSeparator />
          </>
        )}

        <DropdownMenuSub>
          <DropdownMenuSubTrigger disabled={disabled}>
            <Quote className="size-4" />
            <span>Citations</span>
            <span className="text-muted-foreground flex-1 text-right text-xs">
              {citationLabel}
            </span>
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-72 p-0" collisionPadding={8}>
            <div className="flex flex-col gap-1 p-1">
              <DefaultRow
                isDefault={isDefault}
                tooltip={
                  isDefault
                    ? `New turns cite the way Settings says. Currently: ${CITATION_FORMAT_LABELS[defaultCitationFormat]}.`
                    : "Override active. Toggle Default to follow the setting again."
                }
                disabled={Boolean(disabled)}
                onToggle={(useDefault) => {
                  void onCitationFormatChange(
                    useDefault ? null : defaultCitationFormat,
                  );
                }}
              />
              <DropdownMenuSeparator />
              {CITATION_FORMAT_OPTIONS.map((option) => {
                const selected = !isDefault && citationFormat === option.value;
                return (
                  <DropdownMenuItem
                    key={option.value}
                    disabled={disabled}
                    onSelect={(event) => {
                      event.preventDefault();
                      if (selected) return;
                      void onCitationFormatChange(option.value);
                    }}
                    className={cn(isDefault && "opacity-60")}
                  >
                    <span className="text-sm">{option.label}</span>
                    {selected && <Check className="ml-auto size-4" />}
                  </DropdownMenuItem>
                );
              })}
            </div>
          </DropdownMenuSubContent>
        </DropdownMenuSub>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
