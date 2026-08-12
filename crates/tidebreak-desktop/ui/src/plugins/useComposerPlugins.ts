import { useCallback, useMemo } from "react";

import type { ApiClient } from "@/api";
import type { ComposerPlugins } from "@/Composer";
import { slashOptionsFromCatalog, type SlashOption } from "@/ComposerSlash";
import { pluginsApisFromClient } from "./pluginsApis";
import { usePluginCatalog } from "./usePluginCatalog";

/**
 * Everything a composer draws from the plugin library: the bundles its tools
 * menu offers, and the skills and prompts a `/` reaches.
 *
 * One hook over one catalog on purpose — the two surfaces are the same
 * installation read two ways, and a second hook would mean a second
 * `GET /plugins` on every chat view for a list already in hand.
 */
export type ComposerPluginLibrary = {
  /** Absent while loading, after a failed load, or when nothing is installed. */
  plugins: ComposerPlugins | undefined;
  /** Enabled bundles, skills, and prompts, flat, for the composer's panel. */
  slashOptions: SlashOption[];
  loadPromptBody: (name: string) => Promise<string>;
};

/**
 * Picking a bundle from the composer is a shortcut, not a per-chat setting: a
 * bundle being on is still a property of this installation, so engaging one
 * that is off turns it on the same way its switch on the Plugins page would.
 * Nothing is offered while the catalog is loading or after it failed — the
 * composer shows no section rather than reporting on a library the reader did
 * not ask about.
 */
export function useComposerPlugins(client: ApiClient): ComposerPluginLibrary {
  const apis = useMemo(() => pluginsApisFromClient(client), [client]);
  const { catalog, setEnabled } = usePluginCatalog(apis);
  const plugins = catalog?.plugins ?? [];
  const slashOptions = useMemo(
    () => slashOptionsFromCatalog(catalog),
    [catalog],
  );
  const loadPromptBody = useCallback(
    async (name: string) => (await apis.promptBody(name)).body,
    [apis],
  );
  return {
    plugins:
      plugins.length === 0
        ? undefined
        : {
            items: plugins,
            onSelect: (plugin) => {
              if (plugin.enabled) return;
              setEnabled({ plugins: { [plugin.name]: true }, skills: {} });
            },
          },
    slashOptions,
    loadPromptBody,
  };
}
