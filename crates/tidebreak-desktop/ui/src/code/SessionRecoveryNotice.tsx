import type {
  Attention,
  CodeSessionLifecycle,
  FenceReason,
} from "../api/types";
import { recoveryAttention } from "./sessionRecovery";
import { useConfirm } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import { Loader } from "@/components/motion/loader";
import { useRecoveryDelay } from "./useRecoveryDelay";

export function SessionRecoveryNotice({
  lifecycle,
  attention,
  reason,
  retrying = false,
  showProgress = true,
  allowRetry = true,
  onRetry,
}: {
  lifecycle: CodeSessionLifecycle | undefined;
  attention: Attention | undefined;
  reason?: FenceReason;
  retrying?: boolean;
  showProgress?: boolean;
  allowRetry?: boolean;
  onRetry: () => void;
}) {
  attention = recoveryAttention(lifecycle, attention, reason);
  const { confirm, dialog } = useConfirm();
  const recovering =
    lifecycle === "fenced" && attention?.state.type === "fenced";
  const showRecovery = useRecoveryDelay(recovering);
  if (lifecycle !== "fenced") return null;
  if (recovering) {
    if (!showProgress || !showRecovery) return null;
    return (
      <div
        role="status"
        className="mx-4 mt-3 flex items-center gap-2 text-sm text-muted-foreground"
      >
        <Loader
          variant="comet"
          size={12}
          className="text-muted-foreground"
          decorative
        />
        <span>Reconnecting…</span>
      </div>
    );
  }
  if (attention?.state.type !== "needs_you") return null;
  const missingOutput =
    reason === undefined ||
    reason?.type === "terminal_flush_missing" ||
    reason?.type === "incarnation_unresolved" ||
    reason?.type === "sandbox_lost";
  const canRetry = allowRetry && lifecycle === "fenced";
  async function retry() {
    if (
      missingOutput &&
      !(await confirm({
        title: "Continue without the missing output?",
        description:
          "Continue with the saved transcript and release the previous session. Any output that was not saved will remain unavailable. The interrupted turn will not run again.",
        confirmLabel: "Continue with saved transcript",
      }))
    )
      return;
    onRetry();
  }
  return (
    <div
      role="status"
      className="notice-surface notice-warning mx-4 mt-3 flex flex-col gap-2 rounded-md border px-3 py-2 text-sm"
    >
      <p>
        {attention.state.prompt ||
          "This session needs your attention before it can continue."}
      </p>
      {missingOutput && (
        <p className="text-muted-foreground">
          Continuing keeps the saved transcript. The missing final output will
          not be recovered.
        </p>
      )}
      {canRetry && (
        <Button
          type="button"
          size="sm"
          className="self-start"
          disabled={retrying}
          onClick={() => void retry()}
        >
          {retrying
            ? "Retrying…"
            : missingOutput
              ? "Continue with saved transcript"
              : "Retry recovery"}
        </Button>
      )}
      {dialog}
    </div>
  );
}
