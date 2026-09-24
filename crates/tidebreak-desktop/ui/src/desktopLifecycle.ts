import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** The agents a quit prompt is about. */
export type QuitAgentCount = {
  /** Agents not at a safe point. */
  agents: number;
  /**
   * Of those, the ones waiting for the person to answer an approval, a
   * question, or a plan. They reach a safe point only after the answer.
   */
  waitingForYou: number;
};

/**
 * The quit prompt the native shell holds the app's exit on, as `quit.rs`
 * sends it.
 */
export type QuitPromptState =
  | { phase: "idle" }
  | ({ phase: "asking" } & QuitAgentCount)
  | ({ phase: "waiting" } & QuitAgentCount)
  | { phase: "stopping" };

export type QuitPromptUpdate = {
  request: number;
  prompt: QuitPromptState;
  error: string | null;
  /** The person asked to restart, so the app opens again once it quits. */
  restart?: boolean;
};

export type QuitChoice = "stop" | "safe_point" | "cancel";

/** Raised by the shell whenever the quit prompt changes. */
const QUIT_PROMPT_EVENT = "desktop-quit-prompt";

const IDLE: QuitPromptUpdate = {
  request: 0,
  prompt: { phase: "idle" },
  error: null,
};

function isQuitPromptUpdate(value: unknown): value is QuitPromptUpdate {
  if (typeof value !== "object" || value === null) return false;
  const update = value as Record<string, unknown>;
  const prompt = update.prompt as Record<string, unknown> | null;
  if (typeof update.request !== "number" || !prompt) return false;
  if (update.error !== null && typeof update.error !== "string") return false;
  if (update.restart !== undefined && typeof update.restart !== "boolean") {
    return false;
  }
  switch (prompt.phase) {
    case "idle":
    case "stopping":
      return true;
    case "asking":
    case "waiting":
      return (
        typeof prompt.agents === "number" &&
        prompt.agents >= 0 &&
        typeof prompt.waitingForYou === "number" &&
        prompt.waitingForYou >= 0
      );
    default:
      return false;
  }
}

export type QuitPromptController = {
  update: QuitPromptUpdate;
  /** An answer is on its way to the shell; the buttons wait for it. */
  answering: boolean;
  answer: (choice: QuitChoice) => void;
};

/**
 * Follow the shell's quit prompt and carry the person's answer back.
 *
 * Every prompt the shell raises is acknowledged at once. Without that, the
 * shell assumes the renderer cannot show it and asks in a native dialog.
 */
export function useQuitPrompt(): QuitPromptController {
  const [update, setUpdate] = useState<QuitPromptUpdate>(IDLE);
  const [answering, setAnswering] = useState(false);
  const latest = useRef(0);

  const accept = useCallback((next: QuitPromptUpdate) => {
    // Events can land out of order with the mount-time read; the shell
    // numbers them, so the newest wins.
    if (next.request < latest.current) return;
    latest.current = next.request;
    setUpdate(next);
    setAnswering(false);
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    void (async () => {
      try {
        const stop = await listen<unknown>(QUIT_PROMPT_EVENT, ({ payload }) => {
          if (cancelled || !isQuitPromptUpdate(payload)) return;
          accept(payload);
          if (payload.prompt.phase !== "idle") {
            void invoke("quit_prompt_opened", {
              request: payload.request,
            }).catch(() => undefined);
          }
        });
        if (cancelled) stop();
        else unlisten = stop;
        const current = await invoke<unknown>("quit_prompt_state");
        if (!cancelled && isQuitPromptUpdate(current)) accept(current);
      } catch {
        // No prompt is better than a broken one: the shell falls back to
        // asking natively when nothing acknowledges it.
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [accept]);

  const answer = useCallback((choice: QuitChoice) => {
    setAnswering(true);
    void invoke("answer_quit_prompt", { choice }).catch(() => {
      setAnswering(false);
    });
  }, []);

  return { update, answering, answer };
}

/** The previous run of the app, which ended without a clean exit. */
export type UncleanExit = {
  startedAt: string | null;
  version: string | null;
};

export type DiagnosticsSaveState =
  | { status: "idle" }
  | { status: "saving" }
  | { status: "saved" }
  | { status: "failed"; error: string };

export type UncleanExitController = {
  notice: UncleanExit | null;
  save: DiagnosticsSaveState;
  saveReport: () => void;
  dismiss: () => void;
};

function errorText(error: unknown): string {
  if (typeof error === "string" && error) return error;
  if (error instanceof Error && error.message) return error.message;
  return "Could not save the diagnostics report";
}

/**
 * Whether the last run of the app quit unexpectedly, and a way to save a
 * diagnostics report about it.
 */
export function useUncleanExitNotice(): UncleanExitController {
  const [notice, setNotice] = useState<UncleanExit | null>(null);
  const [save, setSave] = useState<DiagnosticsSaveState>({ status: "idle" });

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    void invoke<UncleanExit | null>("unclean_exit_notice")
      .then((value) => {
        if (!cancelled) setNotice(value ?? null);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  const saveReport = useCallback(() => {
    setSave({ status: "saving" });
    void invoke<boolean>("save_diagnostics_report")
      .then((saved) =>
        setSave(saved ? { status: "saved" } : { status: "idle" }),
      )
      .catch((error: unknown) =>
        setSave({ status: "failed", error: errorText(error) }),
      );
  }, []);

  const dismiss = useCallback(() => {
    setNotice(null);
    void invoke("dismiss_unclean_exit_notice").catch(() => undefined);
  }, []);

  return { notice, save, saveReport, dismiss };
}
