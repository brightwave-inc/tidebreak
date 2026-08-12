import type {
  ApiClient,
  PluginCatalog,
  PluginEnableUpdate,
  PromptBody,
  SkillInstructions,
} from "@/api";

/**
 * Everything the Plugins library calls on the server, as one injectable
 * object — the {@link import("@/apps/appsApis").AppsApis} pattern, so the list,
 * the detail, and the toggle round trip are drivable in tests without a
 * network.
 */
export type PluginsApis = {
  list(): Promise<PluginCatalog>;
  setEnabled(update: PluginEnableUpdate): Promise<PluginCatalog>;
  /** One skill's instruction body, fetched when its detail opens. */
  instructions(name: string): Promise<SkillInstructions>;
  /** One prompt's insertable text, fetched when the user picks it. */
  promptBody(name: string): Promise<PromptBody>;
};

export function pluginsApisFromClient(client: ApiClient): PluginsApis {
  return {
    list: () => client.listPlugins(),
    setEnabled: (update) => client.setPluginsEnabled(update),
    instructions: (name) => client.getSkillInstructions(name),
    promptBody: (name) => client.getPromptBody(name),
  };
}
