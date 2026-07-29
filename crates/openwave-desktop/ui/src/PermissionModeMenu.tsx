import { Check, ChevronDown, ShieldCheck, Sparkles, Zap } from "lucide-react";
import type { PermissionMode } from "./api";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";

/**
 * Every mode in ascending order of autonomy, with its menu copy.
 *
 * The descriptions state the decision order the server actually applies:
 * saved allowlists run without asking in every mode, and the mode governs
 * only the calls no grant covers.
 */
const PERMISSION_MODE_SCALE: {
  value: PermissionMode;
  label: string;
  description: string;
  icon: typeof ShieldCheck;
  /** Elevated autonomy gets an accent so it is legible at a glance. */
  elevated: boolean;
}[] = [
  {
    value: "ask",
    label: "Ask",
    description:
      "Ask before edits and actions that leave the workspace. Allowlists you've saved still run without asking.",
    icon: ShieldCheck,
    elevated: false,
  },
  {
    value: "auto",
    label: "Auto",
    description:
      "Workspace edits run on their own; anything that leaves the workspace still asks.",
    icon: Sparkles,
    elevated: true,
  },
  {
    value: "allow",
    label: "Allow all",
    description: "Everything runs without asking, in this chat only.",
    icon: Zap,
    elevated: true,
  },
];

/** The stored mode a chat runs under when none is set. */
export const DEFAULT_PERMISSION_MODE: PermissionMode = "ask";

export function permissionModeOption(mode: PermissionMode | null) {
  const value = mode ?? DEFAULT_PERMISSION_MODE;
  return (
    PERMISSION_MODE_SCALE.find((option) => option.value === value) ??
    PERMISSION_MODE_SCALE[0]
  );
}

/**
 * Per-chat permission-mode selector, shown beside the model picker. `null`
 * reads as Ask; selecting Ask stores the explicit token rather than clearing,
 * so a chat that was deliberately dialed back stays that way if the default
 * ever changes.
 */
export function PermissionModeMenu({
  value,
  disabled,
  onChange,
}: {
  value: PermissionMode | null;
  disabled?: boolean;
  onChange: (mode: PermissionMode) => void | Promise<void>;
}) {
  const current = permissionModeOption(value);
  const CurrentIcon = current.icon;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="model-menu-trigger"
          disabled={disabled}
          aria-label={`Permissions: ${current.label}`}
          title={`Permissions: ${current.label}`}
        >
          <CurrentIcon
            className={cn("size-3.5", current.elevated && "text-warning")}
          />
          <span
            className={cn(
              "model-menu-label",
              current.elevated && "text-warning",
            )}
          >
            {current.label}
          </span>
          <ChevronDown className="size-3.5" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side="top"
        className="model-menu-content w-80 overflow-y-auto p-0"
      >
        <div className="flex flex-col gap-1 p-1">
          {PERMISSION_MODE_SCALE.map((option) => {
            const selected = current.value === option.value;
            const OptionIcon = option.icon;
            return (
              <DropdownMenuItem
                key={option.value}
                disabled={disabled}
                onSelect={(event) => {
                  event.preventDefault();
                  if (selected) return;
                  void onChange(option.value);
                }}
                className="flex items-start gap-2"
              >
                <OptionIcon
                  className={cn(
                    "mt-0.5 size-4 shrink-0",
                    option.elevated && "text-warning",
                  )}
                />
                <span className="flex min-w-0 flex-col">
                  <span className="text-sm">{option.label}</span>
                  <span className="text-muted-foreground text-xs">
                    {option.description}
                  </span>
                </span>
                {selected && <Check className="ml-auto size-4 shrink-0" />}
              </DropdownMenuItem>
            );
          })}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
