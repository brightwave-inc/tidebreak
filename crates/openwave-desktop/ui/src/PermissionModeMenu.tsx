import {
  Check,
  ChevronDown,
  NotebookPen,
  ShieldCheck,
  Sparkles,
  Zap,
} from "lucide-react";
import type { PermissionMode } from "./api";
import { Button } from "@/components/ui/button";
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
const ASK_PERMISSION_MODE_OPTION = {
  value: "ask",
  label: "Ask",
  description:
    "Ask before edits and actions that leave the workspace. Allowlists you've saved still run without asking.",
  icon: ShieldCheck,
  elevated: false,
} as const;

const PERMISSION_MODE_SCALE: {
  value: PermissionMode;
  label: string;
  description: string;
  icon: typeof ShieldCheck;
  /** Elevated autonomy gets an accent so it is legible at a glance. */
  elevated: boolean;
}[] = [
  {
    value: "plan",
    label: "Plan",
    description:
      "Read-only: the agent explores and proposes a plan. Nothing is edited or run until you switch modes.",
    icon: NotebookPen,
    elevated: false,
  },
  ASK_PERMISSION_MODE_OPTION,
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
    ASK_PERMISSION_MODE_OPTION
  );
}

/**
 * Per-chat permission-mode selector, shown beside the model picker. `null`
 * reads as Ask; selecting Ask stores the explicit token rather than clearing,
 * so a chat that was deliberately dialed back stays that way if the default
 * ever changes.
 *
 * An elevated mode accents the trigger's text and icon rather than its
 * background: the row reads as one set of controls, and the one that has been
 * dialed up should stand out within it without becoming a second kind of
 * object.
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
        <Button
          variant="ghost"
          className={cn("h-8 gap-1.5", current.elevated && "text-warning-foreground")}
          disabled={disabled}
          aria-label={`Permissions: ${current.label}`}
        >
          <CurrentIcon
            className={cn(
              "size-4",
              current.elevated ? "text-warning-foreground" : "text-muted-foreground",
            )}
          />
          {current.label}
          <ChevronDown className="size-4 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" side="top" className="w-72">
        {PERMISSION_MODE_SCALE.map((option) => {
          const selected = current.value === option.value;
          const OptionIcon = option.icon;
          return (
            <DropdownMenuItem
              key={option.value}
              disabled={disabled}
              onSelect={() => {
                if (selected) return;
                void onChange(option.value);
              }}
              className="flex flex-col items-start gap-0.5 py-3"
            >
              <div className="flex w-full items-center justify-between">
                <span className="flex items-center gap-2 font-medium">
                  <OptionIcon
                    className={cn(
                      "size-4",
                      option.elevated
                        ? "text-warning-foreground"
                        : "text-muted-foreground",
                    )}
                  />
                  {option.label}
                </span>
                {selected && <Check className="size-4" />}
              </div>
              <span className="text-muted-foreground pl-6 text-xs">
                {option.description}
              </span>
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
