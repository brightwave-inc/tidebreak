import { useCallback, useEffect, useState } from "react";
import { MessagesSquare } from "lucide-react";
import { toast } from "sonner";

import type { ApiClient, CodeGrantSnapshot } from "../api";
import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
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
 * reaches the owner here, not in a notification they may have missed.
 * Routine history is the one exception: grants revoked because a new
 * connect approval replaced them collapse behind a per-workspace
 * disclosure so they cannot crowd out the live entries. A grant revoked
 * for any other reason stays visible inline. The whole-workspace revoke
 * is the boundary the design names against a hostile workspace admin —
 * one press cuts everything that workspace holds.
 *
 * Row titles never show a raw channel identifier. A person row uses the
 * grant's display name, borrows the name another grant in the same
 * workspace stored for the same identity, or falls back to a generic
 * label like "Slack member". The identity itself appears exactly once,
 * as the metadata line under the title, and only when it would not
 * repeat the title.
 */
export function ChannelsPanel({
  client,
  onOpenInferencePreferences,
}: {
  client: ApiClient;
  onOpenInferencePreferences?: (grantId: string) => void;
}) {
  const [grants, setGrants] = useState<CodeGrantSnapshot[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refreshFailed, setRefreshFailed] = useState(false);
  const { confirm, dialog } = useConfirm();

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setGrants(await client.listCodeGrants());
      setRefreshFailed(false);
    } catch (err) {
      setError(String(err));
      setRefreshFailed(true);
    } finally {
      setLoading(false);
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function revokeOne(grant: CodeGrantSnapshot, label: string) {
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
  const disabled = loading || working || refreshFailed;

  return (
    <SettingsPanel
      title="Channels"
      description="Manage the people and Slack workspaces that can reach coding sessions on this machine. Every channel uses the repositories available to this instance’s GitHub App. To change repository access, update the app installation in GitHub."
      busy={loading || working}
    >
      {loading && grants === null ? (
        <p className="text-sm text-muted-foreground" role="status">
          Loading grants…
        </p>
      ) : grants === null ? (
        <div className="flex flex-col items-start gap-3">
          <SettingsError>{error}</SettingsError>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => void reload()}
          >
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
              {group.visible.map((grant) => (
                <GrantRow
                  key={grant.id}
                  grant={grant}
                  group={group}
                  disabled={disabled}
                  onRevoke={revokeOne}
                  onOpenInferencePreferences={onOpenInferencePreferences}
                />
              ))}
            </ul>
            {group.replaced.length > 0 && (
              <Collapsible>
                <CollapsibleTrigger asChild>
                  <Button
                    type="button"
                    variant="link"
                    className="h-auto self-start px-0 text-sm text-muted-foreground hover:text-foreground"
                  >
                    {group.replaced.length === 1
                      ? "1 replaced connection"
                      : `${group.replaced.length} replaced connections`}
                  </Button>
                </CollapsibleTrigger>
                <CollapsibleContent className="pt-2">
                  <ul className="flex flex-col gap-2">
                    {group.replaced.map((grant) => (
                      <GrantRow
                        key={grant.id}
                        grant={grant}
                        group={group}
                        disabled={disabled}
                        onRevoke={revokeOne}
                        onOpenInferencePreferences={onOpenInferencePreferences}
                      />
                    ))}
                  </ul>
                </CollapsibleContent>
              </Collapsible>
            )}
            {group.live > 0 && (
              <Button
                type="button"
                variant="ghost-destructive"
                size="sm"
                className="self-start"
                disabled={disabled}
                onClick={() => void revokeWorkspace(group)}
              >
                Revoke this workspace
              </Button>
            )}
          </SettingsSection>
        ))
      )}
      {grants !== null && error && (
        <div className="notice-surface notice-critical flex flex-col items-start gap-2 rounded-md border px-3 py-2">
          <p className="text-sm break-words" role="alert">
            {error}
          </p>
          {refreshFailed && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={loading || working}
              onClick={() => void reload()}
            >
              Refresh grants
            </Button>
          )}
        </div>
      )}
      {dialog}
    </SettingsPanel>
  );
}

type WorkspaceGroup = {
  channelKind: string;
  workspace: string;
  workspaceName: string;
  live: number;
  /** Live grants plus grants revoked for anything other than replacement. */
  visible: CodeGrantSnapshot[];
  /** Grants revoked because a new connect approval replaced them. */
  replaced: CodeGrantSnapshot[];
  /** Best known display name per external identity across the group. */
  names: Map<string, string>;
};

/**
 * The reason the connect flow writes on the grants a fresh approval
 * replaces. The wire carries no structured discriminator — only this
 * English sentence — so it must stay byte-identical to the literal in
 * `complete_connect_handshake_and_mint_grant_all_owners`
 * (crates/tidebreak-core/src/db/ops/code/connect.rs).
 */
const REPLACED_BY_NEW_CONNECT = "replaced by a new connect approval";

function isReplacedRevocation(grant: CodeGrantSnapshot): boolean {
  return (
    grant.revoked_at != null && grant.revoked_reason === REPLACED_BY_NEW_CONNECT
  );
}

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
    const workspace = key.slice(separator + 1);
    const names = new Map<string, string>();
    for (const grant of bucket) {
      if (grant.kind !== "workspace" && grant.display_name) {
        names.set(grant.external_identity, grant.display_name);
      }
    }
    return {
      channelKind: key.slice(0, separator),
      workspace,
      // Old grants may predate the adapter sending real team names, and
      // some sent the identity back as the name; any grant in the group
      // with a real name names the whole group.
      workspaceName:
        bucket.find(
          (grant) => grant.workspace_name && grant.workspace_name !== workspace,
        )?.workspace_name ?? workspace,
      live: bucket.filter((grant) => !grant.revoked_at).length,
      visible: bucket.filter((grant) => !isReplacedRevocation(grant)),
      replaced: bucket.filter(isReplacedRevocation),
      names,
    };
  });
}

function grantTitle(grant: CodeGrantSnapshot, group: WorkspaceGroup): string {
  if (grant.kind === "workspace") {
    return `Workspace ${group.workspaceName}`;
  }
  return (
    grant.display_name ??
    group.names.get(grant.external_identity) ??
    `${channelLabel(grant.channel_kind)} member`
  );
}

function GrantRow({
  grant,
  group,
  disabled,
  onRevoke,
  onOpenInferencePreferences,
}: {
  grant: CodeGrantSnapshot;
  group: WorkspaceGroup;
  disabled: boolean;
  onRevoke: (grant: CodeGrantSnapshot, label: string) => Promise<void>;
  onOpenInferencePreferences?: (grantId: string) => void;
}) {
  const title = grantTitle(grant, group);
  return (
    <li className="flex flex-col gap-3 rounded-md border px-3 py-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-start gap-3">
          <ChannelAvatar
            grant={grant}
            label={
              grant.kind === "workspace"
                ? (grant.display_name ?? group.workspaceName)
                : title
            }
          />
          <div className="min-w-0">
            <p className="truncate text-sm font-medium">{title}</p>
            {grant.kind !== "workspace" &&
              grant.external_identity !== title && (
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
        {!grant.revoked_at &&
          grant.kind !== "workspace" &&
          onOpenInferencePreferences && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={disabled}
              onClick={() => onOpenInferencePreferences(grant.id)}
            >
              Subscription settings
            </Button>
          )}
        {!grant.revoked_at && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={disabled}
            onClick={() => void onRevoke(grant, title)}
          >
            Revoke
          </Button>
        )}
      </div>
    </li>
  );
}

function ChannelAvatar({
  grant,
  label,
}: {
  grant: CodeGrantSnapshot;
  label: string;
}) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [grant.avatar_url]);
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
