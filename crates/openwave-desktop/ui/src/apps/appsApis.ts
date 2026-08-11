import type {
  ApiClient,
  AppDetail,
  AppGrantState,
  AppLibrary,
  AppFolderInvokeResult,
  AppRestInvokeResult,
  AppViewSessionInfo,
} from "@/api";

/**
 * Everything the Apps library surfaces call on the server, as one injectable
 * object — the {@link import("@/outputs/OutputsView").OutputsApis} pattern,
 * so list, detail, consent, and the open flow are drivable in tests without
 * a network.
 */
export type AppsApis = {
  /** The API origin; frame paths from a view session resolve against it. */
  baseUrl: string;
  list(): Promise<AppLibrary>;
  get(appId: string): Promise<AppDetail>;
  deleteApp(appId: string): Promise<void>;
  grantState(appId: string): Promise<AppGrantState>;
  consent(appId: string): Promise<AppGrantState>;
  revoke(appId: string): Promise<void>;
  viewSession(appId: string): Promise<AppViewSessionInfo>;
  invokeOperation(
    appId: string,
    operationId: string,
    parameters?: unknown,
    body?: unknown,
  ): Promise<AppRestInvokeResult>;
  invokeGatewayOperation(
    appId: string,
    gatewayApp: string,
    operationId: string,
    pathParameters?: unknown,
    query?: unknown,
    body?: unknown,
  ): Promise<AppRestInvokeResult>;
  invokeFolder(
    appId: string,
    folder: string,
    op: "list" | "read" | "write",
    path?: string,
    contentBase64?: string,
    replace?: boolean,
  ): Promise<AppFolderInvokeResult>;
  /**
   * The paired model gateway's origin, or `null` when this profile has none.
   *
   * Only a connect prompt reads it: the gateway's typed
   * `authorization_required` can be resolved nowhere but at the gateway
   * itself, and the handoff is its own SSO in the system browser — no token
   * ever crosses from here.
   */
  gatewayBaseUrl(): Promise<string | null>;
};

export function appsApisFromClient(client: ApiClient): AppsApis {
  return {
    baseUrl: client.baseUrl,
    list: () => client.listApps(),
    get: (appId) => client.getApp(appId),
    deleteApp: (appId) => client.deleteApp(appId),
    grantState: (appId) => client.getAppGrant(appId),
    consent: (appId) => client.consentAppGrant(appId),
    revoke: (appId) => client.revokeAppGrant(appId),
    viewSession: (appId) => client.createAppViewFrame(appId),
    invokeOperation: (appId, operationId, parameters, body) =>
      client.invokeAppOperation(appId, operationId, parameters, body),
    invokeGatewayOperation: (
      appId,
      gatewayApp,
      operationId,
      pathParameters,
      query,
      body,
    ) =>
      client.invokeAppGatewayOperation(
        appId,
        gatewayApp,
        operationId,
        pathParameters,
        query,
        body,
      ),
    invokeFolder: (appId, folder, op, path, contentBase64, replace) =>
      client.invokeAppFolder(appId, folder, op, path, contentBase64, replace),
    gatewayBaseUrl: async () =>
      (await client.getGatewayStatus()).base_url ?? null,
  };
}
