import type {
  ChannelPreferences,
  ChannelPreferencesSnapshot,
  ChannelHarnessCatalog,
} from "../../settings/channelPreferences";
import {
  parseChannelPreferences,
  parseChannelHarnessCatalog,
} from "../../settings/channelPreferences";
import type { CodeConnectPage, CodeGrantSnapshot, HarnessKind } from "../types";
import {
  parseCodeConnectPage,
  parseCodeGrant,
  parseCodeGrantList,
} from "../../code/parsers";
import { type Constructor, HttpCore, requireParsed } from "./http";

/** Code grants and the connect page. */
export function withCodeGrantsApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async getChannelHarnessCatalog(
      grant: string,
      channel: string,
      kind?: HarnessKind,
    ): Promise<ChannelHarnessCatalog> {
      const query = kind ? `?kind=${encodeURIComponent(kind)}` : "";
      return requireParsed(
        parseChannelHarnessCatalog(
          await this.json(
            `/code/grants/${encodeURIComponent(grant)}/channels/${encodeURIComponent(channel)}/harnesses${query}`,
            { headers: this.headers() },
          ),
        ),
        "channel harness catalog",
      );
    }

    async getChannelPreferences(
      grant: string,
      channel: string,
    ): Promise<ChannelPreferencesSnapshot> {
      return requireParsed(
        parseChannelPreferences(
          await this.json(
            `/code/grants/${encodeURIComponent(grant)}/channels/${encodeURIComponent(channel)}/preferences`,
            { headers: this.headers() },
          ),
        ),
        "channel preferences",
      );
    }

    async setChannelPreferences(
      grant: string,
      channel: string,
      preferences: ChannelPreferences,
    ): Promise<ChannelPreferencesSnapshot> {
      return requireParsed(
        parseChannelPreferences(
          await this.json(
            `/code/grants/${encodeURIComponent(grant)}/channels/${encodeURIComponent(channel)}/preferences`,
            {
              method: "PUT",
              headers: this.headers(true),
              body: JSON.stringify(preferences),
            },
          ),
        ),
        "channel preferences",
      );
    }

    /** Every adapter grant the owner holds, revoked rows included. */
    async listCodeGrants(): Promise<CodeGrantSnapshot[]> {
      return requireParsed(
        parseCodeGrantList(
          await this.json("/code/grants", { headers: this.headers() }),
        ),
        "code grants",
      );
    }

    async revokeCodeGrant(
      grantId: string,
      reason?: string,
    ): Promise<CodeGrantSnapshot> {
      return requireParsed(
        parseCodeGrant(
          await this.json(
            `/code/grants/${encodeURIComponent(grantId)}/revoke`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(reason ? { reason } : {}),
            },
          ),
        ),
        "code grant",
      );
    }

    /** Revoke every live grant one channel workspace holds. */
    async revokeCodeGrantWorkspace(
      channelKind: string,
      workspaceIdentity: string,
    ): Promise<CodeGrantSnapshot[]> {
      return requireParsed(
        parseCodeGrantList(
          await this.json("/code/grants/revoke-workspace", {
            method: "POST",
            headers: this.headers(true),
            body: JSON.stringify({
              channel_kind: channelKind,
              workspace_identity: workspaceIdentity,
            }),
          }),
        ),
        "code grants",
      );
    }

    /** What the connect approval page renders; a used or stale link 404s. */
    async getCodeConnectPage(nonce: string): Promise<CodeConnectPage> {
      return requireParsed(
        parseCodeConnectPage(
          await this.json(`/external/connect/${encodeURIComponent(nonce)}`, {
            headers: this.headers(),
          }),
        ),
        "connect page",
      );
    }

    /** The owner's "is this you?". Mints nothing by itself. */
    approveCodeConnect(nonce: string, csrf: string): Promise<void> {
      return this.json(
        `/external/connect/${encodeURIComponent(nonce)}/approve`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify({ csrf }),
        },
      );
    }

    async getWorkspaceGrantPage(id: string): Promise<CodeConnectPage> {
      return requireParsed(
        parseCodeConnectPage(
          await this.json(
            `/deployment/code/grants/workspace/${encodeURIComponent(id)}`,
            { headers: this.headers() },
          ),
        ),
        "workspace grant page",
      );
    }

    approveWorkspaceGrant(id: string, csrf: string): Promise<void> {
      return this.json(
        `/deployment/code/grants/workspace/${encodeURIComponent(id)}/approve`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify({ csrf }),
        },
      );
    }
  };
}
