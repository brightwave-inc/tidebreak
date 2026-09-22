import { useEffect, useState } from "react";

import {
  computerUsePermissionHost,
  type ComputerUsePermissionHost,
  type ComputerUsePermissionStatus,
} from "./computerUsePermissions";

/**
 * Whether this install has already been asked to set up computer use.
 *
 * Per install rather than per account, because the grants are macOS TCC
 * records that belong to this Mac. It sits beside the other desktop-local
 * preferences in `localStorage`, which the webview keeps across updates.
 */
const ASKED_KEY = "tidebreak.computer-use-setup-asked";

export function computerUseSetupAsked(): boolean {
  if (typeof window === "undefined") return true;
  try {
    return window.localStorage.getItem(ASKED_KEY) === "yes";
  } catch {
    // Storage the webview refuses is not a reason to ask on every launch.
    return true;
  }
}

export function markComputerUseSetupAsked(): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(ASKED_KEY, "yes");
  } catch {
    // Best effort. A person who cannot persist the flag sees the dialog again
    // next launch, which is a far smaller problem than failing to show it.
  }
}

/**
 * Whether to open the first-run setup dialog for this status.
 *
 * Only a Mac running Tidebreak locally can hold these grants, so a browser
 * tab, a window attached to another machine, and a Windows or Linux desktop
 * (which answers `unsupported`) never see the dialog. An install that already
 * holds both grants has nothing to ask for.
 */
export function shouldPromptComputerUseSetup(
  availability: ReturnType<ComputerUsePermissionHost["availability"]>,
  status: ComputerUsePermissionStatus,
): boolean {
  if (availability !== "local") return false;
  if (status.status !== "available") return false;
  return !status.accessibility || !status.screenRecording;
}

/**
 * Whether the shell should mount the first-run setup dialog.
 *
 * The status read costs a helper process, so an install that has already been
 * asked never makes it: the stored flag is checked first and answers on its
 * own. A read that fails answers no — a helper that cannot report is not a
 * reason to open a dialog whose buttons would fail too, and the flag stays
 * unset so the next launch can try again.
 */
export function useComputerUseSetupPrompt(
  host: ComputerUsePermissionHost = computerUsePermissionHost,
): boolean {
  const [prompt, setPrompt] = useState(false);

  useEffect(() => {
    if (computerUseSetupAsked()) return;
    const availability = host.availability();
    if (availability !== "local") return;
    let live = true;
    void host
      .status()
      .then((status) => {
        if (!live) return;
        if (shouldPromptComputerUseSetup(availability, status)) {
          setPrompt(true);
        } else if (status.status === "available") {
          // Both grants are already held, so there is nothing to ask and no
          // reason to ask later if one is revoked. Settings owns that case.
          markComputerUseSetupAsked();
        }
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [host]);

  return prompt;
}
