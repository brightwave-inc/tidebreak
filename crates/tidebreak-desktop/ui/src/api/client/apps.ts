import {
  APP_INVOKE_REFUSAL_KINDS,
  type AppDetail,
  type AppFolderInvokeResult,
  type AppGatewayPageResult,
  type AppGrantState,
  AppInvokeRefusalError,
  type AppInvokeRefusalKind,
  type AppLibrary,
  type AppRestInvokeResult,
  type AppViewSession,
  type ConnectedAppsInfo,
  type GatewayApps,
  type GatewayStatus,
  type ManagedPolicy,
  type McpAppPayload,
  type McpServerDefinition,
  type McpServersInfo,
  type McpViewSession,
  type PluginCatalog,
  type PluginEnableUpdate,
  type PromptBody,
  type RestCredentialUpdate,
  type SkillInstructions,
  type SpecDiscoveryInfo,
  type SpecPreviewInfo,
} from "../types";
import { attachedRemotely } from "../../host";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { type Constructor, HttpCore, throwIfNotOk } from "./http";

/** Connected apps, MCP servers, the gateway, plugins, and app invocation. */
export function withAppsApi<TBase extends Constructor<HttpCore>>(Base: TBase) {
  return class extends Base {
    listConnectedApps(): Promise<ConnectedAppsInfo> {
      return this.json("/connected-apps", { headers: this.headers() });
    }

    putRestConnectedApp(
      id: string,
      body: {
        name: string;
        base_url: string;
        /** The raw JSON OpenAPI document, when supplied inline; the server
         * ingests it once here. Exactly one of this and
         * `openapi_document_url` must be present. */
        openapi_document?: string;
        /** URL the server fetches the document from at save time. Requires
         * `document_sha256`. */
        openapi_document_url?: string;
        /** The preview's document hash pin; the save refuses (409) if the
         * document no longer matches it. */
        document_sha256?: string;
        /** When present, only these operationIds are ingested; the rest of
         * the document is not judged. */
        operation_ids?: string[];
        credential: RestCredentialUpdate;
      },
    ): Promise<ConnectedAppsInfo> {
      return this.json(`/connected-apps/rest/${encodeURIComponent(id)}`, {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify(body),
      });
    }

    /** List what an OpenAPI document declares, for the operation picker. */
    previewRestSpec(
      source: { url: string } | { document: string },
    ): Promise<SpecPreviewInfo> {
      return this.json("/connected-apps/rest/spec-preview", {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({ source }),
      });
    }

    /** Probe well-known OpenAPI locations for one https origin or base URL. */
    discoverRestSpec(origin: string): Promise<SpecDiscoveryInfo> {
      return this.json("/connected-apps/rest/spec-discovery", {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({ origin }),
      });
    }

    deleteRestConnectedApp(id: string): Promise<void> {
      return this.json(`/connected-apps/rest/${encodeURIComponent(id)}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    listMcpServers(): Promise<McpServersInfo> {
      return this.json("/mcp/servers", { headers: this.headers() });
    }

    putMcpServers(servers: McpServerDefinition[]): Promise<McpServersInfo> {
      // The native command exists to put an OS confirmation in front of stdio
      // commands that would run on this computer, and it writes to the server
      // embedded in this app. Neither is right while attached: the commands
      // would run on the machine, and writing here would save the reader's
      // edit to a config they were not looking at — `listMcpServers` reads the
      // machine's, so the panel would re-read and show the edit gone.
      if (isTauri() && !attachedRemotely()) {
        return invoke<McpServersInfo>("put_native_mcp_servers", {
          config: { servers },
        });
      }
      return this.json("/mcp/servers", {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify({ servers }),
      });
    }

    reconnectMcpServer(name: string): Promise<McpServersInfo> {
      return this.json(`/mcp/servers/${encodeURIComponent(name)}/reconnect`, {
        method: "POST",
        headers: this.headers(),
      });
    }

    /** Trade the bearer for a single-use iframe address for one view. */
    createMcpViewFrame(server: string, uri: string): Promise<McpViewSession> {
      return this.json(
        `/mcp/servers/${encodeURIComponent(server)}/view-session`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify({ uri }),
        },
      );
    }

    getPolicy(): Promise<ManagedPolicy> {
      return this.json("/policy", { headers: this.headers() });
    }

    getGatewayStatus(): Promise<GatewayStatus> {
      return this.json("/gateway/status", { headers: this.headers() });
    }

    gatewaySignIn(): Promise<{ authorization_url: string }> {
      return this.json("/gateway/sign-in", {
        method: "POST",
        headers: this.headers(),
      });
    }

    gatewaySignOut(): Promise<GatewayStatus> {
      return this.json("/gateway/sign-out", {
        method: "POST",
        headers: this.headers(),
      });
    }

    /** Decline the pending deep-link pairing; returns the policy to render. */
    dismissGatewayPairing(): Promise<ManagedPolicy> {
      return this.json("/gateway/pairing/dismiss", {
        method: "POST",
        headers: this.headers(),
      });
    }

    getGatewayApps(): Promise<GatewayApps> {
      return this.json("/gateway/apps", { headers: this.headers() });
    }

    /**
     * The hosted machine this profile's gateway offers, if it offers one.
     *
     * A hint for the address field, never a grant: attaching runs the same
     * discovery handshake either way. An absent `url` means no prefill — a
     * gateway that hosts no machine, one older than the field, and one that
     * did not answer are the same answer here.
     */
    getGatewayMachine(): Promise<{ url?: string }> {
      return this.json("/gateway/machine", { headers: this.headers() });
    }

    syncGatewayModels(): Promise<GatewayStatus> {
      return this.json("/gateway/models/sync", {
        method: "POST",
        headers: this.headers(),
      });
    }

    getMcpAppPayload(chatId: string, callId: string): Promise<McpAppPayload> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/calls/${encodeURIComponent(callId)}/mcp-app-payload`,
        { headers: this.headers() },
      );
    }

    listApps(): Promise<AppLibrary> {
      return this.json("/apps", { headers: this.headers() });
    }

    listPlugins(): Promise<PluginCatalog> {
      return this.json("/plugins", { headers: this.headers() });
    }

    /** One skill's full instruction body — what the model is taught by it. */
    getSkillInstructions(name: string): Promise<SkillInstructions> {
      return this.json(
        `/plugins/skills/${encodeURIComponent(name)}/instructions`,
        {
          headers: this.headers(),
        },
      );
    }

    /**
     * One prompt's insertable text — what a picker drops into the composer.
     *
     * Its own route because the catalog is read far more often than any single
     * prompt is inserted.
     */
    getPromptBody(name: string): Promise<PromptBody> {
      return this.json(`/plugins/prompts/${encodeURIComponent(name)}/body`, {
        headers: this.headers(),
      });
    }

    /**
     * Set the named enable flags, and take the fresh catalog back.
     *
     * A merge patch: the body names only what is changing, and the response is
     * the authority on what the whole catalog now looks like — which is what
     * lets a surface toggle optimistically and reconcile from one round trip.
     */
    setPluginsEnabled(update: PluginEnableUpdate): Promise<PluginCatalog> {
      return this.json("/plugins/enabled", {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify(update),
      });
    }

    getApp(appId: string): Promise<AppDetail> {
      return this.json(`/apps/${encodeURIComponent(appId)}`, {
        headers: this.headers(),
      });
    }

    deleteApp(appId: string): Promise<void> {
      return this.json(`/apps/${encodeURIComponent(appId)}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    getAppGrant(appId: string): Promise<AppGrantState> {
      return this.json(`/apps/${encodeURIComponent(appId)}/grant`, {
        headers: this.headers(),
      });
    }

    /**
     * Record consent. Deliberately body-less: consent is only ever "yes to what
     * the server shows right now" — the server recomputes the grant from the
     * current manifest and definitions, so a stale sheet can never widen it.
     */
    consentAppGrant(appId: string): Promise<AppGrantState> {
      return this.json(`/apps/${encodeURIComponent(appId)}/grant`, {
        method: "POST",
        headers: this.headers(),
      });
    }

    revokeAppGrant(appId: string): Promise<void> {
      return this.json(`/apps/${encodeURIComponent(appId)}/grant`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    /**
     * Where this app's page lives at the gateway, registering it there first if
     * it has never been registered.
     *
     * Publishing itself happens on that page, not here: it mutates entitlement
     * state the gateway owns, and every other mutation of that state is already
     * done at the gateway (decision record 14).
     *
     * Every way the gateway can decline is an outcome in the response rather
     * than a thrown error: the author asked a legitimate question and the
     * gateway's answer — including its own words — is what the page renders.
     */
    appGatewayPage(appId: string): Promise<AppGatewayPageResult> {
      return this.json(`/apps/${encodeURIComponent(appId)}/gateway-page`, {
        method: "POST",
        headers: this.headers(true),
      });
    }

    /** Trade the bearer for a single-use iframe address for one app revision. */
    createAppViewFrame(appId: string): Promise<AppViewSession> {
      return this.json(`/apps/${encodeURIComponent(appId)}/view-session`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({}),
      });
    }

    /**
     * Execute one of an app's pinned REST operations outside any turn.
     * `parameters`, `body`, and the result are opaque passthrough between the
     * sandboxed frame and the server; a typed refusal surfaces as
     * {@link AppInvokeRefusalError} so the caller can branch on
     * `consent_required` without string-matching prose. The response body
     * crosses base64-encoded in `body_base64` (see {@link AppRestInvokeResult}).
     */
    async invokeAppOperation(
      appId: string,
      operationId: string,
      parameters?: unknown,
      body?: unknown,
    ): Promise<AppRestInvokeResult> {
      const request: Record<string, unknown> = { operation_id: operationId };
      if (parameters !== undefined) request.parameters = parameters;
      if (body !== undefined) request.body = body;
      return (await this.postAppInvoke(appId, request)) as AppRestInvokeResult;
    }

    /**
     * Execute one operation of an app's granted gateway binding — the same
     * invoke route, relayed by the server to the model gateway as the signed-in
     * user. The field names are the gateway's own invoke vocabulary, so a
     * bundle authored here speaks the same shape to the gateway's shell;
     * `path_parameters`, `query`, `body`, and the result are opaque passthrough.
     */
    async invokeAppGatewayOperation(
      appId: string,
      gatewayApp: string,
      operationId: string,
      pathParameters?: unknown,
      query?: unknown,
      body?: unknown,
    ): Promise<AppRestInvokeResult> {
      const request: Record<string, unknown> = {
        gateway_app: gatewayApp,
        operation_id: operationId,
      };
      if (pathParameters !== undefined)
        request.path_parameters = pathParameters;
      if (query !== undefined) request.query = query;
      if (body !== undefined) request.body = body;
      return (await this.postAppInvoke(appId, request)) as AppRestInvokeResult;
    }

    /**
     * Execute one folder operation of an app's granted folder binding — the
     * `folder` sibling of {@link invokeAppOperation}, with the same refusal
     * contract. File content crosses base64-encoded in both directions;
     * failures come back as `is_error` results in the host's closed
     * vocabulary.
     */
    async invokeAppFolder(
      appId: string,
      folder: string,
      op: "list" | "read" | "write",
      path?: string,
      contentBase64?: string,
      replace?: boolean,
    ): Promise<AppFolderInvokeResult> {
      const request: Record<string, unknown> = { folder, op };
      if (path !== undefined) request.path = path;
      if (contentBase64 !== undefined) request.content_base64 = contentBase64;
      if (replace !== undefined) request.replace = replace;
      return (await this.postAppInvoke(
        appId,
        request,
      )) as AppFolderInvokeResult;
    }

    /** The shared invoke POST: one route, typed refusals surfaced as errors. */
    private async postAppInvoke(
      appId: string,
      request: unknown,
    ): Promise<unknown> {
      const response = await fetch(
        `${this.baseUrl}/apps/${encodeURIComponent(appId)}/invoke`,
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify(request),
        },
      );
      if (!response.ok) {
        let refusal: unknown;
        try {
          refusal = await response.clone().json();
        } catch {
          /* not a typed refusal; fall through to the generic error */
        }
        if (
          typeof refusal === "object" &&
          refusal !== null &&
          "kind" in refusal &&
          "message" in refusal &&
          APP_INVOKE_REFUSAL_KINDS.includes(
            (refusal as { kind: AppInvokeRefusalKind }).kind,
          )
        ) {
          const typed = refusal as {
            kind: AppInvokeRefusalKind;
            message: string;
          };
          throw new AppInvokeRefusalError(typed.kind, String(typed.message));
        }
        await throwIfNotOk(response);
      }
      return await response.json();
    }
  };
}
