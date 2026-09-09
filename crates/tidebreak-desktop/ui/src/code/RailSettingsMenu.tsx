import { ListFilter } from "lucide-react";

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
import { useCodeUiStore, type CodeRailPrefs } from "./CodeUiStore";
import { RAIL_ICON_BUTTON } from "./interactive";
import {
  CARD_DENSITIES,
  CARD_DENSITY_LABELS,
  WORKSPACE_SORT_MODE_LABELS,
  WORKSPACE_SORT_MODES,
} from "./workspaceCards";

/** Grouping, density, and card metadata share one settings popover. */

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

export function RailSettingsMenu({
  prefs: suppliedPrefs,
  onPrefsChange,
}: {
  prefs?: CodeRailPrefs;
  onPrefsChange?: (patch: Partial<CodeRailPrefs>) => void;
} = {}) {
  const storedPrefs = useCodeUiStore((state) => state.railPrefs);
  const setStoredPrefs = useCodeUiStore((state) => state.setRailPrefs);
  const prefs = suppliedPrefs ?? storedPrefs;
  const setRailPrefs = onPrefsChange ?? setStoredPrefs;

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={RAIL_ICON_BUTTON}
          aria-label="Workspace list settings"
        >
          <ListFilter size={15} />
        </button>
      </PopoverTrigger>
      <PopoverContent
        side="right"
        align="start"
        sideOffset={8}
        className="flex w-[22rem] max-w-[calc(100vw-24px)] flex-col gap-3 p-3"
      >
        <div className="flex flex-col gap-1.5">
          <span className="text-xs font-medium text-muted-foreground">
            Group by
          </span>
          <SegmentedControl
            aria-label="Group workspaces"
            value={prefs.sortMode}
            onValueChange={(sortMode) => setRailPrefs({ sortMode })}
            options={SORT_OPTIONS}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <span className="text-xs font-medium text-muted-foreground">
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
    <label className="flex cursor-pointer items-center justify-between gap-3 text-md">
      <span>{label}</span>
      <Switch
        checked={checked}
        onCheckedChange={onCheckedChange}
        aria-label={label}
      />
    </label>
  );
}
