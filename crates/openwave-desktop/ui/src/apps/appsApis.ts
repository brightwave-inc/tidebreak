import type {
  ApiClient,
  AppDetail,
  AppGrantState,
  AppInvokeResult,
  AppLibrary,
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
  invoke(appId: string, tool: string, args: unknown): Promise<AppInvokeResult>;
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
    invoke: (appId, tool, args) => client.invokeApp(appId, tool, args),
  };
}
