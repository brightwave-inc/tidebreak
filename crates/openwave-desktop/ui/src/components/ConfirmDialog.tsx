import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactElement,
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
  description?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  destructive?: boolean;
};

type PendingConfirm = ConfirmOptions & {
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

  const activateNext = useCallback(() => {
    const next = queueRef.current.shift() ?? null;
    pendingRef.current = next;
    setPending(next);
  }, []);

  const confirm = useCallback((options: ConfirmOptions) => {
    return new Promise<boolean>((resolve) => {
      queueRef.current.push({ ...options, resolve });
      if (pendingRef.current === null) activateNext();
    });
  }, [activateNext]);

  const settle = useCallback((result: boolean) => {
    const current = pendingRef.current;
    if (!current) return;
    pendingRef.current = null;
    current.resolve(result);
    activateNext();
  }, [activateNext]);

  useEffect(
    () => () => {
      pendingRef.current?.resolve(false);
      pendingRef.current = null;
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
        <AlertDialogContent>
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
