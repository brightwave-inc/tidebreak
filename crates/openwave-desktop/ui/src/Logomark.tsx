import type { ComponentPropsWithoutRef } from "react";

import { cn } from "@/lib/utils";

/** OpenWave mark geometry, shared with the packaged desktop app icon. */
const LOGOMARK_VIEWBOX = "127 217 757 407";
const LOGOMARK_PATH = `
  M1736 3851l-179-178 4-9c2-5 306-309 675-676l671-668h723v18l-1 17-832 838-832 838-25-1-25-1-179-178z
  M6430 2939 5345 1860l546-540h999v25l-300 300c-165 165-300 303-300 307s87 93 193 198l192 191 305-6 1663 1663-14 22-1114-1-1085-1080z
  M3575 2718c208-211 400-404 427-430l48-48v-25l-450-450-755-4-1373-1386 9-15 1604 1 1875 1866v28l-845 845h-919l379-382z
  M6860 2005l-115-116 345-341c190-188 460-456 602-596l256-254 10 3c5 2 159 150 341 328l331 324v22l-745 745h-910l-115-115z
`;

/**
 * The boxed lockup — the mark on its tile, the same shape as the packaged app
 * icon and the form the brand is recognized in.
 *
 * The bare mark is four thin chevrons; at rail size it reads as an ornament
 * rather than as the app. The tile is what makes it a logo, so anywhere the
 * logo stands for the app this is the one to use.
 */
export function BoxedLogomark({
  className,
  ...props
}: ComponentPropsWithoutRef<"span">) {
  return (
    <span
      className={cn(
        "inline-flex size-6 shrink-0 items-center justify-center rounded-[7px] bg-foreground text-background",
        className,
      )}
      {...props}
    >
      <Logomark width="15" height="8" />
    </span>
  );
}

/** The bare mark. Prefer {@link BoxedLogomark} where the logo stands alone. */
export function Logomark(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="24"
      height="13"
      viewBox={LOGOMARK_VIEWBOX}
      fill="currentColor"
      aria-hidden
      {...props}
    >
      <path
        d={LOGOMARK_PATH}
        transform="translate(0 640) scale(.1 -.1)"
      />
    </svg>
  );
}
