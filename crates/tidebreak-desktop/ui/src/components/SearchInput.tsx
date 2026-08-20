import { cva, type VariantProps } from "class-variance-authority";
import { SearchIcon, XIcon } from "lucide-react";
import { type ComponentProps, type RefObject, useRef, useState } from "react";
import { useHotkeys } from "react-hotkeys-hook";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

const variants = cva(
  "flex items-center rounded-lg border border-input bg-transparent transition-colors outline-none focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/25",
  {
    variants: {
      size: {
        sm: "min-h-8 gap-1.5 pr-1 pl-2 text-xs [&_svg]:size-3.5 [&_svg]:shrink-0",
        default: "min-h-control gap-2 pr-1.5 pl-2.5 text-sm [&_svg]:size-4 [&_svg]:shrink-0",
      },
    },
    defaultVariants: { size: "default" },
  },
);

type Props = {
  value?: string;
  onValueChange?: (value: string) => void;
  placeholder?: string;
  inputRef?: RefObject<HTMLInputElement | null>;
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
  inputRef,
  size,
  className,
  ...props
}: Props) {
  const [isFocused, setIsFocused] = useState(false);
  const localRef = useRef<HTMLInputElement | null>(null);
  const ref = inputRef ?? localRef;
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
      <Input
        ref={ref}
        type="search"
        placeholder={placeholder}
        value={value}
        onChange={(event) => onValueChange?.(event.target.value)}
        onFocus={() => setIsFocused(true)}
        onBlur={() => setIsFocused(false)}
        className="h-auto min-w-0 grow border-0 bg-transparent p-0 shadow-none focus-visible:border-transparent focus-visible:ring-0 [&::-webkit-search-cancel-button]:hidden"
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
