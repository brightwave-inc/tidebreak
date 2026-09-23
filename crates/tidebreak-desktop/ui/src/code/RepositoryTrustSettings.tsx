import { useEffect, useRef, useState } from "react";

import type { ApiClient } from "@/api/client";
import type { CodeRepoTrustSnapshot } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  SettingsError,
  SettingsField,
  SettingsSection,
} from "@/settings/primitives";
import { ProjectConfigFileList } from "./RepositoryTrustSheet";

type TrustSettingsClient = Pick<
  ApiClient,
  "getCodeRepoTrust" | "setCodeRepoTrust"
>;

/**
 * Whether engines load the repository's own settings, with the switch that
 * grants or revokes it and the files it covers.
 *
 * The switch saves when it changes, like every other setting. Turning it off
 * is how trust is revoked: an idle session restarts its engine without the
 * settings at once, and a working one after its turn ends.
 */
export function RepositoryTrustSettings({
  client,
  repoId,
}: {
  client: TrustSettingsClient;
  repoId: string | null;
}) {
  const [snapshot, setSnapshot] = useState<CodeRepoTrustSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);

  const load = async () => {
    const token = ++generation.current;
    if (!repoId) {
      setSnapshot(null);
      setLoading(false);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const next = await client.getCodeRepoTrust(repoId);
      if (token === generation.current) setSnapshot(next);
    } catch (caught) {
      if (token === generation.current) {
        setError(
          friendlyErrorMessage(
            caught,
            "Could not read the repository's trust.",
          ),
        );
      }
    } finally {
      if (token === generation.current) setLoading(false);
    }
  };

  useEffect(() => {
    setSnapshot(null);
    void load();
    return () => {
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, repoId]);

  const save = async (trusted: boolean) => {
    if (!repoId) return;
    const token = generation.current;
    setSaving(true);
    setError(null);
    try {
      const next = await client.setCodeRepoTrust(repoId, trusted);
      if (token === generation.current) setSnapshot(next);
    } catch (caught) {
      if (token === generation.current) {
        setError(
          friendlyErrorMessage(
            caught,
            trusted
              ? "Could not trust the repository."
              : "Could not stop trusting the repository.",
          ),
        );
      }
    } finally {
      if (token === generation.current) setSaving(false);
    }
  };

  // The main settings section already explains an unregistered repository.
  if (!repoId) return null;

  const trusted = snapshot?.trust === "trusted";
  return (
    <SettingsSection
      title="Repository trust"
      description="A repository can have its own settings for coding engines, such as hooks, MCP servers, plugins, and instructions. They can run commands on your computer outside Tidebreak's approvals, so engines skip them until you trust the repository."
    >
      {(loading || saving) && (
        <div className="flex items-start justify-end">
          <Spinner aria-hidden="true" className="size-3.5" />
        </div>
      )}
      {error && (
        <div className="flex flex-col items-start gap-2">
          <SettingsError>{error}</SettingsError>
          {!snapshot && (
            <Button
              type="button"
              size="xs"
              variant="outline"
              disabled={loading}
              onClick={() => void load()}
            >
              Try again
            </Button>
          )}
        </div>
      )}
      {snapshot && (
        <>
          <SettingsField
            label="Trust this repository"
            hint={
              trusted
                ? "Engines load this repository's own settings. Turn this off to start sessions without them."
                : "Engines start without this repository's own settings. Turn this on only if you trust everyone who can change the repository."
            }
          >
            <Switch
              checked={trusted}
              disabled={saving}
              onCheckedChange={(next) => void save(next)}
              aria-label="Trust this repository"
            />
          </SettingsField>
          {snapshot.files.length > 0 ? (
            <div className="flex flex-col gap-2">
              <span className="settings-field-label">
                Settings in this checkout
              </span>
              <ProjectConfigFileList files={snapshot.files} />
            </div>
          ) : (
            <p className="settings-field-hint">
              This checkout has no engine settings of its own.
            </p>
          )}
        </>
      )}
    </SettingsSection>
  );
}
