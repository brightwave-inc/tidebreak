import { create } from "zustand";

import type { ComputerUsePermissionPane } from "./computerUsePermissions";

/**
 * Raised by the desktop when a task's computer-use operation stopped because
 * macOS has not granted a permission it needs. `computer_use.rs` sends a
 * {@link PermissionRequired}.
 */
export const PERMISSION_REQUIRED_EVENT = "computer-use-permission-required";

/** A task that stopped for a missing macOS permission. */
export type PermissionRequired = {
  /** The conversation or code session the task runs in. */
  taskId: string;
  /** The missing permission, when the helper named it. */
  permission: ComputerUsePermissionPane | null;
  /** The task was working in a web browser's own window. */
  browser: boolean;
  /**
   * The person had just allowed the task to use the app, which is turning
   * computer use on for it. That is the person acting, so it asks again even
   * after Not now.
   */
  afterConsent: boolean;
};

export function isPermissionRequired(
  value: unknown,
): value is PermissionRequired {
  if (typeof value !== "object" || value === null) return false;
  const event = value as Record<string, unknown>;
  return (
    typeof event.taskId === "string" &&
    (event.permission === null ||
      event.permission === "accessibility" ||
      event.permission === "screen_recording") &&
    typeof event.browser === "boolean" &&
    typeof event.afterConsent === "boolean"
  );
}

/** What the ask is for: other apps on this Mac, or a browser's own window. */
export type PermissionAskSubject = "apps" | "browser";

/** Why the ask is open. */
export type PermissionAsk = {
  subject: PermissionAskSubject;
  /** A task is waiting on the permissions, rather than the person asking. */
  forTask: boolean;
};

/**
 * Where Not now is remembered. Per install, beside the other desktop-local
 * preferences, because the grants are records macOS keeps for this Mac.
 */
const NOT_NOW_KEY = "tidebreak.computer-use-permissions-not-now";

export function permissionAskDeclined(): boolean {
  try {
    return window.localStorage.getItem(NOT_NOW_KEY) === "yes";
  } catch {
    return false;
  }
}

function rememberNotNow(declined: boolean): void {
  try {
    if (declined) window.localStorage.setItem(NOT_NOW_KEY, "yes");
    else window.localStorage.removeItem(NOT_NOW_KEY);
  } catch {
    // Best effort. Without storage, a task can ask again next launch, which
    // is smaller than losing the ask altogether.
  }
}

/**
 * Whether a task that needs a permission opens the ask on its own.
 *
 * The first time it does: that is the moment the permission is needed. After
 * Not now it waits for the person to act again, by allowing a task to use an
 * app or by choosing Allow on the notice, so a task that keeps trying cannot
 * keep asking.
 */
export function asksForTask(
  need: PermissionRequired,
  declined: boolean,
): boolean {
  return need.afterConsent || !declined;
}

type ComputerUsePermissionAskStore = {
  /** The open ask, or `null` while nothing is asking. */
  ask: PermissionAsk | null;
  /**
   * The tasks that stopped for a missing permission, one entry per task and
   * the latest report from each, oldest first. Each gets its own notice.
   */
  needs: PermissionRequired[];
  /**
   * macOS was asked for Screen Recording during this run of the app, by a
   * task or by the person. A grant made after launch applies only once
   * Tidebreak restarts, so this is when the restart is offered.
   */
  screenRecordingRequested: boolean;
  /** A task reported a missing permission. */
  taskNeedsPermission: (need: PermissionRequired) => void;
  /** The person asked to set the permissions up. */
  openAsk: (subject: PermissionAskSubject) => void;
  /** The ask closed: Not now (or Escape), or with both permissions ready. */
  closeAsk: (outcome: "not_now" | "ready") => void;
  /** Put away one task's notice. */
  dismissNeed: (taskId: string) => void;
  noteScreenRecordingRequested: () => void;
};

export const useComputerUsePermissionAsk =
  create<ComputerUsePermissionAskStore>()((set, get) => ({
    ask: null,
    needs: [],
    screenRecordingRequested: false,
    taskNeedsPermission: (need) => {
      // The helper asks macOS for both permissions before it refuses, so a
      // missing Screen Recording has now been requested in this run.
      set({
        needs: [
          ...get().needs.filter((other) => other.taskId !== need.taskId),
          need,
        ],
        screenRecordingRequested: true,
      });
      if (get().ask !== null) return;
      if (!asksForTask(need, permissionAskDeclined())) return;
      if (need.afterConsent) rememberNotNow(false);
      set({
        ask: { subject: need.browser ? "browser" : "apps", forTask: true },
      });
    },
    openAsk: (subject) => {
      rememberNotNow(false);
      set({ ask: { subject, forTask: get().needs.length > 0 } });
    },
    closeAsk: (outcome) => {
      if (outcome === "ready") {
        // Both permissions are in place, so every task can try again.
        rememberNotNow(false);
        set({ ask: null, needs: [] });
      } else {
        rememberNotNow(true);
        set({ ask: null });
      }
    },
    dismissNeed: (taskId) =>
      set({ needs: get().needs.filter((need) => need.taskId !== taskId) }),
    noteScreenRecordingRequested: () => {
      if (!get().screenRecordingRequested) {
        set({ screenRecordingRequested: true });
      }
    },
  }));
