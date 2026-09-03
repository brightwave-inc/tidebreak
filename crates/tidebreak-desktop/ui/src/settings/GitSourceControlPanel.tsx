import { useEffect, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "@/api/client";
import type { BranchPrefixMode, RuntimeSettings } from "@/api/types";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";
import { PortableConfigSection } from "./PortableConfigSection";

type GitSettingsClient = Pick<
  ApiClient,
  | "getSettings"
  | "putSettings"
  | "exportWorkspaceConfig"
  | "previewWorkspaceConfig"
  | "applyWorkspaceConfig"
>;
type GitSettings = RuntimeSettings["git_source_control"];

function prefixStem(prefix: string | undefined): string {
  return prefix?.replace(/\/+$/, "") ?? "";
}

export function GitSourceControlPanel({
  client,
}: {
  client: GitSettingsClient;
}) {
  const [settings, setSettings] = useState<GitSettings | null>(null);
  const [customPrefix, setCustomPrefix] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void client
      .getSettings()
      .then((next) => {
        if (cancelled) return;
        setSettings(next.git_source_control);
        setCustomPrefix(
          prefixStem(next.git_source_control.custom_branch_prefix),
        );
      })
      .catch((caught: unknown) => {
        if (!cancelled) {
          setError(
            friendlyErrorMessage(caught, "Could not load Git settings."),
          );
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  async function save(
    update: Parameters<
      GitSettingsClient["putSettings"]
    >[0]["git_source_control"],
    rollback?: { settings: GitSettings; customPrefix: string },
  ) {
    if (!update) return;
    setSaving(true);
    setError(null);
    try {
      const next = await client.putSettings({ git_source_control: update });
      setSettings(next.git_source_control);
      setCustomPrefix(prefixStem(next.git_source_control.custom_branch_prefix));
    } catch (caught) {
      if (rollback) {
        setSettings(rollback.settings);
        setCustomPrefix(rollback.customPrefix);
      }
      const message = friendlyErrorMessage(
        caught,
        "Could not save Git settings.",
      );
      setError(message);
      toast.error(message);
    } finally {
      setSaving(false);
    }
  }

  function selectMode(mode: BranchPrefixMode) {
    if (!settings) return;
    const rollback = { settings, customPrefix };
    if (mode === "custom") {
      const custom =
        customPrefix || prefixStem(settings.account_prefix) || "tidebreak";
      setCustomPrefix(custom);
      setSettings({
        ...settings,
        branch_prefix_mode: mode,
        custom_branch_prefix: `${custom}/`,
        effective_branch_prefix: `${custom}/`,
      });
      void save(
        { branch_prefix_mode: mode, custom_branch_prefix: custom },
        rollback,
      );
      return;
    }
    setSettings({
      ...settings,
      branch_prefix_mode: mode,
      effective_branch_prefix:
        mode === "none" ? "" : (settings.account_prefix ?? "tidebreak/"),
    });
    void save({ branch_prefix_mode: mode }, rollback);
  }

  const previewPrefix =
    settings?.branch_prefix_mode === "custom"
      ? `${customPrefix.trim().replace(/\/+$/, "")}${customPrefix.trim() ? "/" : ""}`
      : (settings?.effective_branch_prefix ?? "");

  return (
    <SettingsPanel
      title="Git & source control"
      description="Choose how Tidebreak names branches and worktree folders for new code workspaces."
      busy={loading || saving}
    >
      {error && <SettingsError>{error}</SettingsError>}
      {loading && !settings ? (
        <p className="text-sm text-muted-foreground">Loading Git settings…</p>
      ) : (
        settings && (
          <SettingsSection
            title="Branch names"
            description="These defaults apply when you add a repository. Existing repositories keep their own branch prefix."
          >
            <SettingsField
              label="Name generated branches and folders automatically"
              hint="When a new workspace starts with a message, Tidebreak names its local branch and worktree folder before creating them. Existing paths never move."
            >
              <Switch
                checked={settings.auto_rename_branches}
                disabled={saving}
                onCheckedChange={(enabled) => {
                  const rollback = { settings, customPrefix };
                  setSettings({ ...settings, auto_rename_branches: enabled });
                  void save({ auto_rename_branches: enabled }, rollback);
                }}
                aria-label="Name generated branches and folders automatically"
              />
            </SettingsField>
            <SettingsField
              label="Branch prefix"
              hint={
                settings.branch_prefix_mode === "account" &&
                !settings.account_prefix
                  ? "No Git account is available, so Tidebreak uses tidebreak/."
                  : "The prefix is copied into each repository you add."
              }
            >
              <Select
                value={settings.branch_prefix_mode}
                disabled={saving}
                onValueChange={(value) => selectMode(value as BranchPrefixMode)}
              >
                <SelectTrigger aria-label="Branch prefix">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="account">
                    Git account
                    {settings.account_prefix
                      ? ` (${prefixStem(settings.account_prefix)})`
                      : ""}
                  </SelectItem>
                  <SelectItem value="custom">Custom</SelectItem>
                  <SelectItem value="none">No prefix</SelectItem>
                </SelectContent>
              </Select>
            </SettingsField>
            {settings.branch_prefix_mode === "custom" && (
              <SettingsField
                label="Custom prefix"
                hint="Use a valid Git branch prefix. Tidebreak adds the trailing slash."
              >
                <Input
                  className="font-mono"
                  aria-label="Custom prefix"
                  value={customPrefix}
                  disabled={saving}
                  spellCheck={false}
                  placeholder="team/alex"
                  onChange={(event) => setCustomPrefix(event.target.value)}
                  onBlur={() => {
                    if (customPrefix.trim()) {
                      void save({ custom_branch_prefix: customPrefix });
                    }
                  }}
                />
              </SettingsField>
            )}
            <div className="rounded-lg border border-border-subtle bg-muted px-3 py-2.5">
              <p className="text-xs text-muted-foreground">Example branch</p>
              <p className="mt-1 truncate font-mono text-sm text-foreground">
                {previewPrefix}fix-flaky-auth-retry
              </p>
            </div>
          </SettingsSection>
        )
      )}
      <PortableConfigSection client={client} />
    </SettingsPanel>
  );
}
