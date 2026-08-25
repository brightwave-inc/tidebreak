import { Loader2Icon } from "lucide-react";
import { forwardRef, type HTMLAttributes, type ReactNode } from "react";

import { cn } from "@/lib/utils";
import { DOCUMENT_VIEWER_SURFACE } from "./extendViewerSurface";

export const DocumentViewerShell = forwardRef<
  HTMLDivElement,
  HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    className={cn(
      "relative flex min-h-0 flex-col overflow-hidden",
      DOCUMENT_VIEWER_SURFACE,
      className,
    )}
    {...props}
  />
));
DocumentViewerShell.displayName = "DocumentViewerShell";

type DocumentViewerStateProps = HTMLAttributes<HTMLDivElement> & {
  variant?: "loading" | "error" | "message";
  children: ReactNode;
};

/** A consistent status surface for every document engine and file format. */
export function DocumentViewerState({
  variant = "message",
  className,
  children,
  role,
  ...props
}: DocumentViewerStateProps) {
  const resolvedRole =
    role ??
    (variant === "error"
      ? "alert"
      : variant === "loading"
        ? "status"
        : undefined);

  return (
    <div
      className={cn(
        "flex min-h-64 grow items-center justify-center p-6 text-center text-sm text-muted-foreground",
        className,
      )}
      role={resolvedRole}
      aria-busy={variant === "loading" || undefined}
      {...props}
    >
      <div className="flex max-w-sm flex-col items-center gap-2 text-balance">
        {variant === "loading" ? (
          <Loader2Icon className="size-6 animate-spin" aria-hidden="true" />
        ) : null}
        <div>{children}</div>
      </div>
    </div>
  );
}
