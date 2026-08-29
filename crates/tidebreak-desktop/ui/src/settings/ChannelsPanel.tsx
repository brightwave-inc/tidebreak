import { useCallback, useEffect, useState } from "react";
import { MessagesSquare } from "lucide-react";
import { toast } from "sonner";

import type { ApiClient, CodeGrantSnapshot } from "../api";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { useConfirm } from "@/components/ConfirmDialog";
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
  const [loading, setLoading] = useState(true);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setGrants(await client.listCodeGrants());
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function revokeOne(grant: CodeGrantSnapshot) {
    const label = grant.display_name ?? grant.external_identity;
    const accepted = await confirm({
      title: `Revoke ${label}?`,
      description: `This immediately cuts this ${channelLabel(grant.channel_kind)} account off from every coding session it can reach on this machine.`,
      confirmLabel: "Revoke",
      destructive: true,
    });
    if (!accepted) return;
    setWorking(true);
    setError(null);
    try {
      await client.revokeCodeGrant(grant.id);
      toast.success(`Revoked ${label}`);
      await reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  async function revokeWorkspace(group: WorkspaceGroup) {
    const accepted = await confirm({
      title: `Revoke every grant from ${group.workspaceName}?`,
      description: `This immediately cuts off ${group.live} connected ${group.live === 1 ? "person" : "people"} from this ${channelLabel(group.channelKind)} workspace.`,
      confirmLabel: "Revoke workspace",
      destructive: true,
    });
    if (!accepted) return;
    setWorking(true);
    setError(null);
    try {
      const revoked = await client.revokeCodeGrantWorkspace(
        group.channelKind,
        group.workspace,
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
      busy={loading || working}
    >
      {loading && grants === null ? (
        <p className="text-sm text-muted-foreground" role="status">
          Loading grants…
        </p>
      ) : grants === null ? (
        <div className="flex flex-col items-start gap-3">
          <SettingsError>{error}</SettingsError>
          <Button type="button" variant="outline" size="sm" onClick={reload}>
            Try again
          </Button>
        </div>
      ) : groups.length === 0 ? (
        <Empty className="min-h-64">
          <EmptyHeader>
            <EmptyMedia variant="icon" className="text-icon-green">
              <MessagesSquare />
            </EmptyMedia>
            <EmptyTitle>No channels connected</EmptyTitle>
            <EmptyDescription>
              Connecting starts in the channel. In Slack, mention the agent and
              follow its connect link.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        groups.map((group) => (
          <SettingsSection
            key={`${group.channelKind}:${group.workspace}`}
            title={`${channelLabel(group.channelKind)} · ${group.workspaceName}`}
            description={
              group.live > 0
                ? `Workspace ${group.workspace}. Revoking the workspace cuts every live grant it holds.`
                : "Every grant this workspace held is revoked."
            }
          >
            <ul className="flex flex-col gap-2">
              {group.grants.map((grant) => (
                <li
                  key={grant.id}
                  className="flex flex-wrap items-center justify-between gap-2 rounded-md border px-3 py-2"
                >
                  <div className="flex min-w-0 items-start gap-3">
                    <ChannelAvatar grant={grant} />
                    <div className="min-w-0">
                      <p className="truncate text-sm font-medium">
                        {grant.display_name ?? grant.external_identity}
                      </p>
                      {grant.display_name && (
                        <p className="truncate font-mono text-xs text-muted-foreground">
                          {grant.external_identity}
                        </p>
                      )}
                      <p className="text-xs text-muted-foreground">
                        {grant.revoked_at
                          ? `Revoked ${formatDay(grant.revoked_at)} — ${grant.revoked_reason ?? "no reason recorded"}`
                          : `Connected ${formatDay(grant.created_at)}`}
                      </p>
                    </div>
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
                onClick={() => void revokeWorkspace(group)}
              >
                Revoke this workspace
              </Button>
            )}
          </SettingsSection>
        ))
      )}
      {grants !== null && error && <SettingsError>{error}</SettingsError>}
      {dialog}
    </SettingsPanel>
  );
}

type WorkspaceGroup = {
  channelKind: string;
  workspace: string;
  workspaceName: string;
  live: number;
  grants: CodeGrantSnapshot[];
};

function groupByWorkspace(grants: CodeGrantSnapshot[]): WorkspaceGroup[] {
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
      workspaceName: bucket[0]?.workspace_name ?? key.slice(separator + 1),
      live: bucket.filter((grant) => !grant.revoked_at).length,
      grants: bucket,
    };
  });
}

function ChannelAvatar({ grant }: { grant: CodeGrantSnapshot }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [grant.avatar_url]);
  const label = grant.display_name ?? grant.external_identity;
  if (!grant.avatar_url || failed) {
    return (
      <span
        className="grid size-8 shrink-0 place-items-center rounded-full bg-muted text-xs font-semibold uppercase text-muted-foreground"
        aria-hidden
      >
        {label.slice(0, 2)}
      </span>
    );
  }
  return (
    <img
      src={grant.avatar_url}
      alt=""
      className="size-8 shrink-0 rounded-full object-cover"
      referrerPolicy="no-referrer"
      decoding="async"
      onError={() => setFailed(true)}
    />
  );
}

function channelLabel(kind: string): string {
  return kind === "slack" ? "Slack" : kind;
}

function formatDay(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? timestamp : date.toLocaleDateString();
}
