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
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";
import { ExternalEditorSection } from "./ExternalEditorSection";
import { WorktreeRootSection } from "./WorktreeRootSection";
import { Switch } from "@/components/ui/switch";

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
 * an engine, and the folder only matters once one runs. The external editor
 * follows it for the same reason — it is where the files a workspace produces
 * go next.
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
  const [rewriteClosing, setRewriteClosing] = useState(false);
  const [savingRewrite, setSavingRewrite] = useState(false);

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

  useEffect(() => {
    let cancelled = false;
    void client
      .getSettings()
      .then((settings) => {
        if (!cancelled) setRewriteClosing(settings.rewrite_closing_messages);
      })
      .catch(() => {
        // The toggle stays off until a later load succeeds.
      });
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
      <ExternalEditorSection canDetect={hasLocalHostAuthority()} />
      <SettingsSection
        title="Transcript"
        description="A completed turn can rewrite its closing message into lucid prose. The original stays; you can switch between them."
      >
        <SettingsField
          label="Rewrite closing messages"
          hint="Uses the utility model after each completed turn. Off by default."
        >
          <Switch
            checked={rewriteClosing}
            disabled={savingRewrite}
            onCheckedChange={(enabled) => {
              setRewriteClosing(enabled);
              setSavingRewrite(true);
              void client
                .putSettings({ rewrite_closing_messages: enabled })
                .then((settings) => {
                  setRewriteClosing(settings.rewrite_closing_messages);
                })
                .catch((caught: unknown) => {
                  setRewriteClosing(!enabled);
                  toast.error(friendlyErrorMessage(caught));
                })
                .finally(() => setSavingRewrite(false));
            }}
            aria-label="Rewrite closing messages"
          />
        </SettingsField>
      </SettingsSection>
    </SettingsPanel>
  );
}
