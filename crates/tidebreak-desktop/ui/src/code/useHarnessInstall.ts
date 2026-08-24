import { useEffect } from "react";

import type { ApiClient } from "../api/client";
import type { CodeHarnessInstallSnapshot, HarnessKind } from "../api/types";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

/** What this hook needs from the client; a test can pass two functions. */
export type HarnessInstallClient = Pick<
  ApiClient,
  "startHarnessInstall" | "getHarnessDoctor"
>;

/**
 * Whether a client carries both halves of an install.
 *
 * Surfaces that take an optional, narrowed client — the workspace start
 * prompt, and every test that stubs one method — reach the hook through this
 * rather than a cast.
 */
export function canInstallHarnesses(
  client: Partial<HarnessInstallClient> | undefined,
): client is HarnessInstallClient {
  return Boolean(client?.startHarnessInstall && client?.getHarnessDoctor);
}

/**
 * Download the engine a picker has landed on, if this machine does not have
 * it yet.
 *
 * A pinned engine is a 37-297MB `npm install`. Tidebreak used to install all
 * four before any of them could be used, because the doctor's one install
 * control fetched every pin. Now nothing is fetched until a reader picks it,
 * which puts the download on the surface that knows which engine is next
 * rather than on create, where it was a silent multi-minute stall.
 *
 * Returns the install's own progress so the caller can say what the wait is.
 * Ready engines return `undefined` and nothing is requested.
 */
export function useWarmHarnessInstall(
  /** Undefined on a surface with no client, which downloads nothing. */
  client: HarnessInstallClient | undefined,
  harness: HarnessKind | undefined,
  /** False while the surface is closed, so a hidden picker downloads nothing. */
  active: boolean,
  /** The doctor's answer for `harness`. False starts the download. */
  installed: boolean,
): CodeHarnessInstallSnapshot | undefined {
  const reloadDoctor = useCodeCatalogStore((state) => state.reloadDoctor);
  const installs = useCodeUpdatesStore((state) => state.harnessInstalls);
  const install = harness ? installs[harness] : undefined;
  const wanted = Boolean(active && client && harness && !installed);

  useEffect(() => {
    if (!wanted || !client || !harness) return;
    let cancelled = false;
    void client.startHarnessInstall(harness).then(
      (snapshot) => {
        // The answer is immediate; the phases after it arrive on the updates
        // socket. Applying it here means the note shows even on the profile
        // that never opened one.
        if (!cancelled) {
          useCodeUpdatesStore
            .getState()
            .apply({ type: "harness_install", install: snapshot });
        }
      },
      // Nothing is broken yet: create still reports `harness_not_found` with
      // the reason, and a member of a shared deployment may not install at
      // all. A toast on picker open would be noise either way.
      () => {},
    );
    return () => {
      cancelled = true;
    };
  }, [client, harness, wanted]);

  useEffect(() => {
    // A finished install leaves the doctor report saying the engine is not
    // installed, so it would stay unstartable until something re-read it.
    if (!wanted || !client || !install?.done || install.error) return;
    void reloadDoctor(client).catch(() => {});
  }, [client, install, reloadDoctor, wanted]);

  return install;
}
