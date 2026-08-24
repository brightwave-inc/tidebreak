import { CircleAlertIcon } from "lucide-react";
import type { PropsWithChildren } from "react";

export function DocumentError(props: PropsWithChildren) {
  return (
    <div
      className="flex flex-col items-center justify-center gap-3 px-6 py-10 text-center text-sm text-muted-foreground"
      role="alert"
    >
      <CircleAlertIcon className="size-5" aria-hidden="true" />
      {props.children}
    </div>
  );
}
