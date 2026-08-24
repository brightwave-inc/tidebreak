import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type {
  CodeWorktreeRoot,
  HarnessDoctorReport,
  HarnessKind,
} from "../api/types";
import { DoctorList } from "@/code/DoctorList";
import { useCodeUpdatesStore } from "@/code/CodeUpdatesStore";
import { hasLocalHostAuthority, pickCodeDirectory } from "@/host";
import { friendlyErrorMessage } from "@/lib/utils";
import { hostMachineLabel } from "@/remoteMachine";
import { SettingsError, SettingsPanel } from "./primitives";
import { WorktreeRootSection } from "./WorktreeRootSection";

/**
 * Settings: the coding-harness doctor, and where workspaces land on disk.
 *
 * Every engine's state, and the control that downloads one, live here so a
 * reader can get an engine working without opening a workspace. Downloads run
 * one engine at a time and report on the same live bus the pickers watch, so
 * starting one here and then opening the New Workspace dialog shows the same
 * progress in both places.
 *
 * The workspace folder sits below the engines: a reader who came here came for
 * an engine, and the folder only matters once one runs.
 */

export function CodingHarnessesPanel({ client }: { client: ApiClient }) {
  const [report, setReport] = useState<HarnessDoctorReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [worktreeRoot, setWorktreeRoot] = useState<CodeWorktreeRoot | null>(
    null,
  );
  const [rootDraft, setRootDraft] = useState("");
  const [savingRoot, setSavingRoot] = useState(false);

  async function load(refresh: boolean) {
    if (refresh) setRefreshing(true);
    else setLoading(true);
    setError(null);
    try {
      const next = refresh
        ? await client.refreshHarnessDoctor()
        : await client.getHarnessDoctor();
      setReport(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }

  useEffect(() => {
    void load(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const next = await client.getCodeWorktreeRoot();
        if (cancelled) return;
        setWorktreeRoot(next);
        setRootDraft(next.root ?? "");
      } catch (err) {
        if (!cancelled) setError(String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client]);

  const installs = useCodeUpdatesStore((state) => state.harnessInstalls);

  async function install(kind: HarnessKind) {
    try {
      const snapshot = await client.startHarnessInstall(kind, true);
      useCodeUpdatesStore
        .getState()
        .apply({ type: "harness_install", install: snapshot });
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not start the download"));
    }
  }

  // A finished download leaves the report saying the engine is missing, so
  // read it again rather than making the reader press Re-check. Each install
  // is answered once: a pin that reports `ready` while its probe still finds
  // nothing is a real fault for Re-check to surface, not a reload loop.
  const reloadedFor = useRef(new Set<string>());
  useEffect(() => {
    const landed = Object.values(installs).filter(
      (item) => item?.phase === "ready",
    );
    const fresh = landed.filter(
      (item) =>
        item && !reloadedFor.current.has(`${item.kind}:${item.version}`),
    );
    if (fresh.length === 0) return;
    for (const item of fresh) {
      if (item) reloadedFor.current.add(`${item.kind}:${item.version}`);
    }
    void client
      .getHarnessDoctor()
      .then(setReport)
      .catch(() => {});
  }, [client, installs]);

  async function saveRoot(root: string | null) {
    setSavingRoot(true);
    setError(null);
    try {
      const next = await client.setCodeWorktreeRoot(root);
      setWorktreeRoot(next);
      setRootDraft(next.root ?? "");
      toast.success(
        root === null
          ? "New workspaces use the default folder"
          : "New workspaces land in the new folder",
      );
    } catch (err) {
      setError(friendlyErrorMessage(err, "Could not set the workspace folder"));
    } finally {
      setSavingRoot(false);
    }
  }

  return (
    <SettingsPanel
      title="Coding harnesses"
      description={`Coding engines on ${hostMachineLabel()}. Each one downloads the first time you pick it, so you only pay for the ones you use.`}
      busy={loading || refreshing}
    >
      {error && <SettingsError>{error}</SettingsError>}
      {report && (
        <DoctorList
          report={report}
          title="Engines"
          onRefresh={() => void load(true)}
          refreshing={refreshing}
          onInstall={(kind) => void install(kind)}
          installs={installs}
        />
      )}
      {worktreeRoot && (
        <WorktreeRootSection
          value={rootDraft}
          effectiveRoot={worktreeRoot.effective_root}
          defaultRoot={worktreeRoot.default_root}
          inherited={worktreeRoot.root === undefined}
          busy={savingRoot}
          canBrowse={hasLocalHostAuthority()}
          onChange={setRootDraft}
          onBrowse={() => {
            void (async () => {
              const picked = await pickCodeDirectory();
              if (picked) setRootDraft(picked);
            })();
          }}
          onSave={() => void saveRoot(rootDraft.trim())}
          onReset={() => void saveRoot(null)}
        />
      )}
    </SettingsPanel>
  );
}
