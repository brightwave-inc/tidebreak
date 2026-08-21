import { SlidersHorizontal } from "lucide-react";

import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  SegmentedControl,
  type SegmentedOption,
} from "@/components/ui/segmented";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { useCodeUiStore, type CodeRailPrefs } from "./CodeUiStore";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import {
  CARD_DENSITIES,
  CARD_DENSITY_LABELS,
  WORKSPACE_SORT_MODE_LABELS,
  WORKSPACE_SORT_MODES,
} from "./workspaceCards";

/**
 * The rail's one settings surface: order, density, and card metadata.
 *
 * A popover with real controls, not a menu: sort and density are radio
 * choices and the meta rows are switches, none of which a `DropdownMenu`
 * models without faking. Every control writes through `setRailPrefs`, so a
 * choice made here is the choice every window opens with.
 */

const SORT_OPTIONS: readonly SegmentedOption<CodeRailPrefs["sortMode"]>[] =
  WORKSPACE_SORT_MODES.map((mode) => ({
    value: mode,
    label: WORKSPACE_SORT_MODE_LABELS[mode],
  }));

const DENSITY_OPTIONS: readonly SegmentedOption<CodeRailPrefs["density"]>[] =
  CARD_DENSITIES.map((density) => ({
    value: density,
    label: CARD_DENSITY_LABELS[density],
  }));

export function RailSettingsMenu() {
  const prefs = useCodeUiStore((state) => state.railPrefs);
  const setRailPrefs = useCodeUiStore((state) => state.setRailPrefs);

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={cn(
            "text-muted-foreground hover:bg-muted hover:text-foreground cursor-pointer rounded-md p-1",
            FOCUS_RING,
            HOVER_TINT,
          )}
          aria-label="Workspace list settings"
        >
          <SlidersHorizontal size={14} />
        </button>
      </PopoverTrigger>
      <PopoverContent
        side="right"
        align="start"
        sideOffset={8}
        className="flex w-[22rem] max-w-[calc(100vw-24px)] flex-col gap-3 p-3"
      >
        <div className="flex flex-col gap-1.5">
          <span className="text-[11px] font-medium text-muted-foreground">
            Sort
          </span>
          <SegmentedControl
            aria-label="Sort workspaces"
            value={prefs.sortMode}
            onValueChange={(sortMode) => setRailPrefs({ sortMode })}
            options={SORT_OPTIONS}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <span className="text-[11px] font-medium text-muted-foreground">
            Cards
          </span>
          <SegmentedControl
            aria-label="Card density"
            value={prefs.density}
            onValueChange={(density) => setRailPrefs({ density })}
            options={DENSITY_OPTIONS}
          />
        </div>
        <div className="flex flex-col gap-2">
          <PrefSwitch
            label="Repo on cards"
            checked={prefs.showRepoChip}
            onCheckedChange={(showRepoChip) => setRailPrefs({ showRepoChip })}
          />
          <PrefSwitch
            label="Branch on cards"
            checked={prefs.showBranch}
            onCheckedChange={(showBranch) => setRailPrefs({ showBranch })}
          />
        </div>
      </PopoverContent>
    </Popover>
  );
}

function PrefSwitch({
  label,
  checked,
  onCheckedChange,
}: {
  label: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center justify-between gap-3 text-[13px]">
      <span>{label}</span>
      <Switch
        checked={checked}
        onCheckedChange={onCheckedChange}
        aria-label={label}
      />
    </label>
  );
}
