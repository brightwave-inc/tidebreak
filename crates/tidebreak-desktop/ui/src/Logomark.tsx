import type { ComponentPropsWithoutRef } from "react";

/**
 * Tidebreak mark geometry, shared with the packaged desktop app icon.
 *
 * The box is tight to the drawing, unlike the packaged icon's square canvas:
 * call sites that size the mark with `height: auto` derive their height from
 * this ratio, so squaring it here would pad every one of them. A caller that
 * wants a square footprint asks for one by passing equal width and height.
 */
const LOGOMARK_VIEWBOX = "127 217 757 407";
const LOGOMARK_PATH = `
  M1736 3851l-179-178 4-9c2-5 306-309 675-676l671-668h723v18l-1 17-832 838-832 838-25-1-25-1-179-178z
  M6430 2939 5345 1860l546-540h999v25l-300 300c-165 165-300 303-300 307s87 93 193 198l192 191 305-6 1663 1663-14 22-1114-1-1085-1080z
  M3575 2718c208-211 400-404 427-430l48-48v-25l-450-450-755-4-1373-1386 9-15 1604 1 1875 1866v28l-845 845h-919l379-382z
  M6860 2005l-115-116 345-341c190-188 460-456 602-596l256-254 10 3c5 2 159 150 341 328l331 324v22l-745 745h-910l-115-115z
`;

/**
 * The mark, drawn in the current text color.
 *
 * No tile behind it and no color of its own: the mark takes the foreground,
 * so it reads as ink on the page in the light theme and inverts with it in the
 * dark one, the same way the app's other glyphs do.
 */
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
      <path d={LOGOMARK_PATH} transform="translate(0 640) scale(.1 -.1)" />
    </svg>
  );
}
