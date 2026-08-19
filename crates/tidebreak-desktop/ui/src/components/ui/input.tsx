import * as React from "react";
import { cn } from "@/lib/utils";

type InputProps = Omit<React.ComponentProps<"input">, "size"> & {
  size?: "default" | "sm";
};

const Input = React.forwardRef<HTMLInputElement, InputProps>(
  ({ className, type, size = "default", ...props }, ref) => {
    return (
      <input
        type={type}
        className={cn(
          "flex h-control w-full rounded-lg border border-input bg-transparent px-2.5 py-1 text-base transition-colors outline-none file:border-0 file:bg-transparent file:text-sm file:font-medium file:text-foreground placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/25 disabled:cursor-not-allowed disabled:bg-input/40 disabled:opacity-50 md:text-sm",
          size === "sm" && "h-control-sm rounded-md px-2 py-1 text-xs",
          className,
        )}
        ref={ref}
        {...props}
      />
    );
  },
);
Input.displayName = "Input";

export { Input };
