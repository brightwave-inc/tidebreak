import { CircleAlertIcon } from "lucide-react";
import type { PropsWithChildren } from "react";

export function DocumentError(props: PropsWithChildren) {
  return (
    <div className="flex flex-col items-center justify-center gap-4 py-10 font-bold text-muted-foreground">
      <CircleAlertIcon className="size-4" />
      {props.children}
    </div>
  );
}
