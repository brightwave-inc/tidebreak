import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";

import type { ApiClient, CodeGrantSnapshot } from "../api";
import { Button } from "@/components/ui/button";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

/**
 * The grants an external channel holds on this machine, grouped by the
 * channel workspace that holds them (docs/slack-sessions.md, stage 2).
 *
 * Revoked grants stay listed with their reasons: a theft-triggered revoke
 * reaches the owner here, not in a notification they may have missed. The
 * whole-workspace revoke is the boundary the design names against a
 * hostile workspace admin — one press cuts everything that workspace
 * holds.
 */
export function ChannelsPanel({ client }: { client: ApiClient }) {
  const [grants, setGrants] = useState<CodeGrantSnapshot[] | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setError(null);
    try {
      setGrants(await client.listCodeGrants());
    } catch (err) {
      setError(String(err));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function revokeOne(grant: CodeGrantSnapshot) {
    setWorking(true);
    setError(null);
    try {
      await client.revokeCodeGrant(grant.id);
      toast.success(`Revoked ${grant.external_identity}`);
      await reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  async function revokeWorkspace(channelKind: string, workspace: string) {
    setWorking(true);
    setError(null);
    try {
      const revoked = await client.revokeCodeGrantWorkspace(
        channelKind,
        workspace,
      );
      toast.success(
        revoked.length === 1
          ? "Revoked 1 grant"
          : `Revoked ${revoked.length} grants`,
      );
      await reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  const groups = groupByWorkspace(grants ?? []);

  return (
    <SettingsPanel
      title="Channels"
      description="External channels that can reach coding sessions on this machine. Each grant is one linked person in one channel workspace; revoking it cuts their access immediately."
      busy={grants === null}
    >
      {grants === null ? (
        <p className="text-sm text-muted-foreground">Loading grants…</p>
      ) : groups.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No channel is connected. Connecting starts in the channel itself — for
          Slack, mention the agent and follow its connect link.
        </p>
      ) : (
        groups.map((group) => (
          <SettingsSection
            key={`${group.channelKind}:${group.workspace}`}
            title={`${channelLabel(group.channelKind)} · ${group.workspace}`}
            description={
              group.live > 0
                ? "A grant is trust in the person and in their workspace's administration. Revoking the workspace cuts every grant it holds."
                : "Every grant this workspace held is revoked."
            }
          >
            <ul className="flex flex-col gap-2">
              {group.grants.map((grant) => (
                <li
                  key={grant.id}
                  className="flex flex-wrap items-center justify-between gap-2 rounded-md border px-3 py-2"
                >
                  <div className="min-w-0">
                    <p className="text-sm font-medium">
                      {grant.external_identity}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      {grant.revoked_at
                        ? `Revoked ${formatDay(grant.revoked_at)} — ${grant.revoked_reason ?? "no reason recorded"}`
                        : `Connected ${formatDay(grant.created_at)}`}
                    </p>
                  </div>
                  {!grant.revoked_at && (
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      disabled={working}
                      onClick={() => void revokeOne(grant)}
                    >
                      Revoke
                    </Button>
                  )}
                </li>
              ))}
            </ul>
            {group.live > 0 && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="self-start"
                disabled={working}
                onClick={() =>
                  void revokeWorkspace(group.channelKind, group.workspace)
                }
              >
                Revoke this workspace
              </Button>
            )}
          </SettingsSection>
        ))
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

function groupByWorkspace(grants: CodeGrantSnapshot[]): {
  channelKind: string;
  workspace: string;
  live: number;
  grants: CodeGrantSnapshot[];
}[] {
  const groups = new Map<string, CodeGrantSnapshot[]>();
  for (const grant of grants) {
    const key = `${grant.channel_kind}:${grant.workspace_identity}`;
    const bucket = groups.get(key) ?? [];
    bucket.push(grant);
    groups.set(key, bucket);
  }
  return [...groups.entries()].map(([key, bucket]) => {
    const separator = key.indexOf(":");
    return {
      channelKind: key.slice(0, separator),
      workspace: key.slice(separator + 1),
      live: bucket.filter((grant) => !grant.revoked_at).length,
      grants: bucket,
    };
  });
}

function channelLabel(kind: string): string {
  return kind === "slack" ? "Slack" : kind;
}

function formatDay(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? timestamp : date.toLocaleDateString();
}
