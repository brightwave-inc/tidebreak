import type { ComponentPropsWithoutRef } from 'react';
import { cn } from '@/lib/utils';

/**
 * The Tidebreak mark, vendored from `assets/tidebreak-mark.svg` at the repo root
 * and flattened so it renders standalone (no `<defs>`/`<use>` indirection) and
 * inherits `currentColor`.
 */
function TidebreakMark({ className, ...props }: ComponentPropsWithoutRef<'svg'>) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="127 217 757 407"
      fill="currentColor"
      role="img"
      aria-label="Tidebreak"
      className={cn('h-5 w-auto', className)}
      {...props}
    >
      <g transform="translate(0 640) scale(.1 -.1)">
        <path d="M1736 3851l-179-178 4-9c2-5 306-309 675-676l671-668h723v18l-1 17-832 838-832 838-25-1-25-1-179-178z" />
        <path d="M6430 2939 5345 1860l546-540h999v25l-300 300c-165 165-300 303-300 307s87 93 193 198l192 191 305-6 1663 1663-14 22-1114-1-1085-1080z" />
        <path d="M3575 2718c208-211 400-404 427-430l48-48v-25l-450-450-755-4-1373-1386 9-15 1604 1 1875 1866v28l-845 845h-919l379-382z" />
        <path d="M6860 2005l-115-116 345-341c190-188 460-456 602-596l256-254 10 3c5 2 159 150 341 328l331 324v22l-745 745h-910l-115-115z" />
      </g>
    </svg>
  );
}

export function TidebreakLogo({
  className,
  ...props
}: ComponentPropsWithoutRef<'span'>) {
  return (
    <span
      className={cn('inline-flex items-center gap-2 text-foreground', className)}
      {...props}
    >
      <TidebreakMark />
      <span className="text-[15px] leading-none tracking-tight">
        <span className="font-semibold">Tidebreak</span>
        <span className="ml-1.5 text-muted-foreground">Docs</span>
      </span>
    </span>
  );
}
