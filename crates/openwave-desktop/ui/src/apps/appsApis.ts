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
  invokeFolder(
    appId: string,
    folder: string,
    op: "list" | "read" | "write",
    path?: string,
    contentBase64?: string,
    replace?: boolean,
  ): Promise<AppFolderInvokeResult>;
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
    invokeFolder: (appId, folder, op, path, contentBase64, replace) =>
      client.invokeAppFolder(appId, folder, op, path, contentBase64, replace),
  };
}
