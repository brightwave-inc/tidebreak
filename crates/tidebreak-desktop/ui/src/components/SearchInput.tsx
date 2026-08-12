import { cva, type VariantProps } from "class-variance-authority";
import { SearchIcon, XIcon } from "lucide-react";
import { type ComponentProps, useRef, useState } from "react";
import { useHotkeys } from "react-hotkeys-hook";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const variants = cva(
  "flex items-center rounded-md border border-border bg-background ring-offset-background focus-within:ring-2 focus-within:ring-ring focus-within:ring-offset-2 focus-within:outline-hidden",
  {
    variants: {
      size: {
        sm: "min-h-8 gap-1.5 pr-1 pl-2 text-xs [&_svg]:size-3.5 [&_svg]:shrink-0",
        default: "min-h-10 gap-2 pr-2 pl-3 text-sm [&_svg]:size-4 [&_svg]:shrink-0",
      },
    },
    defaultVariants: { size: "default" },
  },
);

type Props = {
  value?: string;
  onValueChange?: (value: string) => void;
  placeholder?: string;
} & VariantProps<typeof variants> &
  Omit<ComponentProps<"label">, "size">;

/**
 * The one search box every list wears: magnifier, input, and a clear button
 * that holds its space so the row does not reflow as the reader types.
 *
 * Escape clears and blurs while focused, which is why focus is tracked here
 * rather than left to the browser.
 */
export function SearchInput({
  value,
  onValueChange,
  placeholder,
  size,
  className,
  ...props
}: Props) {
  const [isFocused, setIsFocused] = useState(false);
  const ref = useRef<HTMLInputElement | null>(null);
  const empty = (value ?? "").length === 0;

  useHotkeys(
    "esc",
    () => {
      if (!isFocused) return;
      onValueChange?.("");
      ref.current?.blur();
    },
    { enableOnFormTags: true },
  );

  return (
    <label className={cn(variants({ size, className }))} {...props}>
      <SearchIcon
        className={cn("transition-colors", empty ? "text-muted-foreground" : "text-primary")}
      />
      <input
        ref={ref}
        type="search"
        placeholder={placeholder}
        value={value}
        onChange={(event) => onValueChange?.(event.target.value)}
        onFocus={() => setIsFocused(true)}
        onBlur={() => setIsFocused(false)}
        className="min-w-0 grow bg-transparent outline-hidden placeholder:text-muted-foreground [&::-webkit-search-cancel-button]:hidden"
      />
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        aria-label="Clear search"
        className={cn("shrink-0", empty && "invisible")}
        onClick={() => onValueChange?.("")}
      >
        <XIcon />
      </Button>
    </label>
  );
}
