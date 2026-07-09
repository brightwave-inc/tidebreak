import type { ComponentPropsWithoutRef } from "react";

/** Compact mark paths (same geometry as the desktop app icon). */
const LOGOMARK_VIEWBOX = "0 0 24 26";
const LOGOMARK_PATHS = [
  "M17.0903 17.9921L16.4053 18.6724L10.5812 12.8885V12.141L17.7783 4.99361L18.3106 4.77453H21.8286C22.1224 5.06646 22.2881 5.23053 22.5825 5.52246V11.3953L21.8319 12.1406H17.843L17.0903 12.8882V17.9921ZM0 4.77453L0.670741 4.10323L4.30427 7.71165H5.36904L12.631 0.5H13.6954L17.091 3.87221V4.61986L9.73846 11.9216L9.20622 12.1406H0.753842L0.00113062 11.3931V11.3811M0 13.6477V20.2541L0.673709 20.9232L4.30442 17.3177H5.36904L12.634 24.532H13.6983L17.091 21.1629V20.4093L9.73846 13.1074L9.20622 12.8885H0.753842L0.00113062 13.636V13.6479L0 13.6477Z",
  "M18.5956 12.8887H21.8295L22.5822 13.6362V19.5068L21.8295 20.2543H18.5956L17.8428 19.5068V13.6362L18.5956 12.8887Z",
] as const;

/** Logomark only — used in the shell chrome. */
export function Logomark(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="24"
      height="26"
      viewBox={LOGOMARK_VIEWBOX}
      fill="currentColor"
      aria-hidden
      {...props}
    >
      {LOGOMARK_PATHS.map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  );
}
