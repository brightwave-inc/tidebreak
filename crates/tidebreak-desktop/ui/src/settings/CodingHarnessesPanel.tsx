import { useEffect, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type { CodeWorktreeRoot, HarnessDoctorReport } from "../api/types";
import { DoctorList } from "@/code/DoctorList";
import { hasNativeHost, pickCodeDirectory } from "@/host";
import { friendlyErrorMessage } from "@/lib/utils";
import { SettingsError, SettingsPanel } from "./primitives";
import { WorktreeRootSection } from "./WorktreeRootSection";

/**
 * Settings: the coding-harness doctor, and where workspaces land on disk.
 *
 * Found, path, version, tier, capabilities, auth, remediation, and
 * unrecognized-event counts live here so a reader can repair an engine
 * without opening a workspace. The workspace folder sits above them because it
 * decides where the user's own code ends up, which matters before any engine
 * runs.
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
      description="Engines installed on this machine, what they can do, and where the workspaces they run in live."
      busy={loading || refreshing}
    >
      {error && <SettingsError>{error}</SettingsError>}
      {worktreeRoot && (
        <WorktreeRootSection
          value={rootDraft}
          effectiveRoot={worktreeRoot.effective_root}
          defaultRoot={worktreeRoot.default_root}
          inherited={worktreeRoot.root === undefined}
          busy={savingRoot}
          canBrowse={hasNativeHost()}
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
      {report && (
        <DoctorList
          report={report}
          onRefresh={() => void load(true)}
          refreshing={refreshing}
        />
      )}
    </SettingsPanel>
  );
}
