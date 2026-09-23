import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";

export type ConfirmOptions = {
  title: string;
  description?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  destructive?: boolean;
};

export type DecideOptions = ConfirmOptions & {
  /**
   * A second way forward beside the confirm button, for when the reader may
   * want something gentler: Archive beside Delete. It sits between Cancel
   * and the confirm button and never takes the destructive style.
   */
  alternativeLabel: string;
};

/** Which button closed a {@link DecideOptions} dialog. */
export type Decision = "confirm" | "alternative" | "cancel";

type PendingConfirm = ConfirmOptions & {
  id: number;
  alternativeLabel?: string;
  resolve: (value: Decision) => void;
};

/**
 * Promise-based confirmation backed by the shared AlertDialog. Overlapping
 * requests are queued in call order so every returned promise is settled by
 * the dialog that belongs to it.
 *
 * `confirm` asks a yes-or-no question. `decide` adds one alternative, and
 * answers with the button the reader chose. Both are the same dialog.
 */
export function useConfirm(): {
  confirm: (options: ConfirmOptions) => Promise<boolean>;
  decide: (options: DecideOptions) => Promise<Decision>;
  dialog: ReactElement;
} {
  const [pending, setPending] = useState<PendingConfirm | null>(null);
  const pendingRef = useRef<PendingConfirm | null>(null);
  const queueRef = useRef<PendingConfirm[]>([]);
  const buttonResultRef = useRef<Decision | null>(null);
  const nextIdRef = useRef(0);
  const phaseRef = useRef<"idle" | "open" | "closing">("idle");

  const activateNext = useCallback(() => {
    const next = queueRef.current.shift() ?? null;
    pendingRef.current = next;
    phaseRef.current = next ? "open" : "idle";
    setPending(next);
  }, []);

  const decide = useCallback(
    (options: ConfirmOptions & { alternativeLabel?: string }) => {
      return new Promise<Decision>((resolve) => {
        queueRef.current.push({ ...options, id: ++nextIdRef.current, resolve });
        if (phaseRef.current === "idle") activateNext();
      });
    },
    [activateNext],
  );

  const confirm = useCallback(
    async (options: ConfirmOptions) =>
      (await decide({ ...options, alternativeLabel: undefined })) === "confirm",
    [decide],
  );

  const settle = useCallback((result: Decision) => {
    const current = pendingRef.current;
    if (!current) return;
    pendingRef.current = null;
    phaseRef.current = "closing";
    setPending(null);
    current.resolve(result);
  }, []);

  // Render a fully closed dialog before activating the next queued request.
  // This gives Radix a close commit in which to restore focus, so the next
  // request starts on its safe Cancel control instead of reusing the previous
  // destructive action button.
  useEffect(() => {
    if (pending === null && phaseRef.current === "closing") activateNext();
  }, [activateNext, pending]);

  useEffect(
    () => () => {
      pendingRef.current?.resolve("cancel");
      pendingRef.current = null;
      phaseRef.current = "idle";
      for (const queued of queueRef.current.splice(0)) {
        queued.resolve("cancel");
      }
    },
    [],
  );

  const dialog = (
    <AlertDialog
      open={pending !== null}
      onOpenChange={(open) => {
        if (!open) {
          const result = buttonResultRef.current ?? "cancel";
          buttonResultRef.current = null;
          settle(result);
        }
      }}
    >
      {pending && (
        <AlertDialogContent key={pending.id}>
          <AlertDialogHeader>
            <AlertDialogTitle>{pending.title}</AlertDialogTitle>
            {pending.description && (
              <AlertDialogDescription>
                {pending.description}
              </AlertDialogDescription>
            )}
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel
              onClick={() => {
                buttonResultRef.current = "cancel";
              }}
            >
              {pending.cancelLabel ?? "Cancel"}
            </AlertDialogCancel>
            {pending.alternativeLabel && (
              <AlertDialogAction
                variant="outline"
                onClick={() => {
                  buttonResultRef.current = "alternative";
                }}
              >
                {pending.alternativeLabel}
              </AlertDialogAction>
            )}
            <AlertDialogAction
              variant={pending.destructive ? "destructive" : "default"}
              onClick={() => {
                buttonResultRef.current = "confirm";
              }}
            >
              {pending.confirmLabel ?? "Confirm"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      )}
    </AlertDialog>
  );

  return { confirm, decide, dialog };
}
