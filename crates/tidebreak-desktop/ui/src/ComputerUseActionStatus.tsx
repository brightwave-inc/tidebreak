import { Check, Hand, MousePointer2, TriangleAlert } from "lucide-react";
import { STATUS_MARK, STATUS_TEXT, type StatusTone } from "@/code/statusTone";
import { cn } from "@/lib/utils";
import {
  computerUseActionLabel,
  computerUseModeLabel,
  isVisibleComputerUseAction,
  type ComputerUseAction,
} from "./computerUseAction";

/** Status stays outside the native webview, where the shell can render it. */
export function ComputerUseActionStatus({
  action,
  now = Date.now(),
  className,
  reserveSpace = false,
}: {
  action: ComputerUseAction | null;
  now?: number;
  className?: string;
  /** Keep native viewport bounds stable while an action appears or expires. */
  reserveSpace?: boolean;
}) {
  const reservedClass = "h-control shrink-0 overflow-hidden";
  if (!isVisibleComputerUseAction(action, now))
    return reserveSpace ? (
      <div
        aria-hidden
        className={cn(
          "border-b border-border-subtle bg-background",
          reservedClass,
          className,
        )}
      />
    ) : null;
  const tone: StatusTone =
    action.phase === "foreground_required"
      ? "warning"
      : action.phase === "failed"
        ? "critical"
        : action.phase === "completed"
          ? "neutral"
          : "running";
  const Icon =
    action.phase === "foreground_required"
      ? Hand
      : action.phase === "failed"
        ? TriangleAlert
        : action.phase === "completed"
          ? Check
          : MousePointer2;
  return (
    <div
      role="status"
      className={cn(
        "flex min-w-0 items-start gap-2 border-b border-border-subtle bg-background px-3 py-2 text-xs",
        reserveSpace && reservedClass,
        reserveSpace && "items-center py-0",
        className,
      )}
    >
      <Icon
        aria-hidden
        className={cn("mt-px size-3.5 shrink-0", STATUS_MARK[tone])}
      />
      <div
        className={cn(
          "min-w-0",
          reserveSpace && "flex items-center overflow-hidden",
        )}
      >
        <span
          className={cn(
            "font-medium",
            STATUS_TEXT[tone],
            reserveSpace && "shrink-0 whitespace-nowrap",
          )}
        >
          {computerUseActionLabel(action)}
        </span>
        <span
          className={cn(
            "ml-2 text-muted-foreground",
            reserveSpace && "truncate",
          )}
        >
          {computerUseModeLabel(action)}
        </span>
      </div>
    </div>
  );
}
