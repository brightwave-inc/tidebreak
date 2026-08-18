import { useEffect } from "react";
import { toast } from "sonner";
import { Toaster } from "tidebreak-desktop-ui";

/** Backdrop so the toast reads as an overlay over app chrome, not a floating chip. */
function AppBackdrop({ label }: { label: string }) {
  return (
    <div
      style={{
        height: "100%",
        minHeight: 280,
        border: "1px solid var(--border)",
        borderRadius: 8,
        padding: 16,
        background: "var(--page-background)",
        color: "var(--muted-foreground)",
        fontSize: "0.8125rem",
        fontWeight: 500,
      }}
    >
      {label}
    </div>
  );
}

export function TurnFinished() {
  useEffect(() => {
    toast.success("Pull request opened", {
      description: "PR #2183 · tb/fix-retry-test → main",
      duration: Infinity,
    });
  }, []);

  return (
    <>
      <AppBackdrop label="Fix flaky retry test — Claude Code · turn 9" />
      <Toaster />
    </>
  );
}

export function PushFailed() {
  useEffect(() => {
    toast.error("Could not push tb/settings-schema", {
      description: "The remote rejected the push: main has moved on. Rebase and try again.",
      duration: Infinity,
    });
  }, []);

  return (
    <>
      <AppBackdrop label="Migrate settings schema — Codex · turn 14" />
      <Toaster />
    </>
  );
}

export function ToastStack() {
  useEffect(() => {
    toast.success("Agent resumed", { duration: Infinity });
    toast.warning("Prompt cache may not be reused", {
      description: "Switching models mid-chat re-sends the transcript.",
      duration: Infinity,
    });
    toast.error("Terminal exited with code 1", { duration: Infinity });
  }, []);

  return (
    <>
      <AppBackdrop label="tidebreak — 3 workspaces" />
      <Toaster expand />
    </>
  );
}
