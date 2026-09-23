import type { CodePrMergeMethod } from "../api/types";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/** Short labels so both PR merge surfaces can share one control. */
export function MergeMethodSelect({
  value,
  onChange,
  disabled,
}: {
  value: CodePrMergeMethod;
  onChange: (method: CodePrMergeMethod) => void;
  disabled?: boolean;
}) {
  return (
    <Select
      value={value}
      onValueChange={(next) => onChange(next as CodePrMergeMethod)}
      disabled={disabled}
    >
      <SelectTrigger
        size="sm"
        className="w-auto shrink-0"
        aria-label="Merge method"
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="squash">Squash</SelectItem>
        <SelectItem value="merge">Merge</SelectItem>
        <SelectItem value="rebase">Rebase</SelectItem>
      </SelectContent>
    </Select>
  );
}

export function mergeMethodActionLabel(method: CodePrMergeMethod): string {
  switch (method) {
    case "squash":
      return "Squash and merge";
    case "merge":
      return "Create merge commit";
    case "rebase":
      return "Rebase and merge";
  }
}
