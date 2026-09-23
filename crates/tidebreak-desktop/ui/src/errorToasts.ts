import { toast } from "sonner";

/** Error and warning toasts stay until the reader dismisses them. */
const STICKY = {
  duration: Number.POSITIVE_INFINITY,
  closeButton: true,
} as const;

let patched = false;

/**
 * Sonner's default duration is four seconds and there is no close button.
 * Success toasts keep that default; error and warning toasts do not.
 */
export function persistErrorToasts(): void {
  if (patched) return;
  patched = true;
  const error = toast.error.bind(toast);
  const warning = toast.warning.bind(toast);
  toast.error = ((message, data) =>
    error(message, { ...STICKY, ...data })) as typeof toast.error;
  toast.warning = ((message, data) =>
    warning(message, { ...STICKY, ...data })) as typeof toast.warning;
}
