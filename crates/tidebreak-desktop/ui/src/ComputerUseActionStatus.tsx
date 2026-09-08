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
}: {
  action: ComputerUseAction | null;
  now?: number;
  className?: string;
}) {
  if (!isVisibleComputerUseAction(action, now)) return null;
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
        className,
      )}
    >
      <Icon
        aria-hidden
        className={cn("mt-px size-3.5 shrink-0", STATUS_MARK[tone])}
      />
      <div className="min-w-0">
        <span className={cn("font-medium", STATUS_TEXT[tone])}>
          {computerUseActionLabel(action)}
        </span>
        <span className="ml-2 text-muted-foreground">
          {computerUseModeLabel(action)}
        </span>
      </div>
    </div>
  );
}
