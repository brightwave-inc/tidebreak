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

type PendingConfirm = ConfirmOptions & {
  id: number;
  resolve: (value: boolean) => void;
};

/**
 * Promise-based confirmation backed by the shared AlertDialog. Overlapping
 * requests are queued in call order so every returned promise is settled by
 * the dialog that belongs to it.
 */
export function useConfirm(): {
  confirm: (options: ConfirmOptions) => Promise<boolean>;
  dialog: ReactElement;
} {
  const [pending, setPending] = useState<PendingConfirm | null>(null);
  const pendingRef = useRef<PendingConfirm | null>(null);
  const queueRef = useRef<PendingConfirm[]>([]);
  const buttonResultRef = useRef<boolean | null>(null);
  const nextIdRef = useRef(0);
  const phaseRef = useRef<"idle" | "open" | "closing">("idle");

  const activateNext = useCallback(() => {
    const next = queueRef.current.shift() ?? null;
    pendingRef.current = next;
    phaseRef.current = next ? "open" : "idle";
    setPending(next);
  }, []);

  const confirm = useCallback(
    (options: ConfirmOptions) => {
      return new Promise<boolean>((resolve) => {
        queueRef.current.push({ ...options, id: ++nextIdRef.current, resolve });
        if (phaseRef.current === "idle") activateNext();
      });
    },
    [activateNext],
  );

  const settle = useCallback((result: boolean) => {
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
      pendingRef.current?.resolve(false);
      pendingRef.current = null;
      phaseRef.current = "idle";
      for (const queued of queueRef.current.splice(0)) queued.resolve(false);
    },
    [],
  );

  const dialog = (
    <AlertDialog
      open={pending !== null}
      onOpenChange={(open) => {
        if (!open) {
          const result = buttonResultRef.current ?? false;
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
                buttonResultRef.current = false;
              }}
            >
              {pending.cancelLabel ?? "Cancel"}
            </AlertDialogCancel>
            <AlertDialogAction
              variant={pending.destructive ? "destructive" : "default"}
              onClick={() => {
                buttonResultRef.current = true;
              }}
            >
              {pending.confirmLabel ?? "Confirm"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      )}
    </AlertDialog>
  );

  return { confirm, dialog };
}
