import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";

import type { ApiClient } from "../api";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { useExperimentalFlags } from "@/experimental";
import { friendlyErrorMessage } from "@/lib/utils";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

/**
 * Features that are usable but not settled: each row is one opt-in switch.
 *
 * The panel is the writer of record for the experimental flags store — the
 * toggle persists through `PUT /settings` first and only then updates the
 * store, so a rail never shows a surface the server did not accept.
 */
export function ExperimentalPanel({ client }: { client: ApiClient }) {
  const navigate = useNavigate();
  const codeModeEnabled = useExperimentalFlags((state) => state.codeModeEnabled);
  const setCodeModeEnabled = useExperimentalFlags(
    (state) => state.setCodeModeEnabled,
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The shell seeds the store at boot; re-reading on mount keeps the panel
  // honest after another window changed the flag.
  useEffect(() => {
    void useExperimentalFlags.getState().refresh(client);
  }, [client]);

  async function toggleCodeMode(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      const settings = await client.putSettings({ code_mode_enabled: enabled });
      setCodeModeEnabled(settings.code_mode_enabled);
    } catch (err) {
      setError(friendlyErrorMessage(err, "Could not save the setting"));
      toast.error("Could not save the setting");
    } finally {
      setSaving(false);
    }
  }

  return (
    <SettingsPanel
      title="Experimental"
      description="Features that work but are still settling. Each is off until you turn it on, and turning one off hides it again without deleting anything."
      busy={saving}
    >
      <SettingsSection>
        <div className="flex items-start justify-between gap-4">
          <div className="flex flex-col gap-1">
            <span className="font-bold">Code mode</span>
            <span className="text-muted-foreground text-sm">
              Drive coding agents — Claude Code, Codex CLI, opencode, Grok CLI —
              in isolated worktree workspaces on your repositories, with
              approvals, per-turn diffs, and a pull-request flow.
            </span>
          </div>
          <Switch
            aria-label="Code mode"
            checked={codeModeEnabled}
            disabled={saving}
            onCheckedChange={(checked) => void toggleCodeMode(checked)}
          />
        </div>
        {codeModeEnabled && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            onClick={() => void navigate({ to: "/code" })}
          >
            Open code mode
          </Button>
        )}
        {error && <SettingsError>{error}</SettingsError>}
      </SettingsSection>
    </SettingsPanel>
  );
}
