import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import type {
  PluginCatalog,
  PluginEnableUpdate,
  PluginSkillInfo,
} from "@/api";
import type { PluginsApis } from "./pluginsApis";

/**
 * The catalog as it would read if `update` had already been accepted.
 *
 * Applied to the current catalog so a switch moves under the finger rather
 * than after the round trip. It is deliberately the same merge-patch shape the
 * route takes — a name the update omits is left exactly as it was — so this
 * and the server can never disagree about what a body meant. A member skill's
 * own flag is set wherever it appears: inside its bundle, or standing alone.
 */
export function applyEnableUpdate(
  catalog: PluginCatalog,
  update: PluginEnableUpdate,
): PluginCatalog {
  const applySkill = (skill: PluginSkillInfo): PluginSkillInfo => {
    const next = update.skills[skill.name];
    return next === undefined ? skill : { ...skill, enabled: next };
  };
  return {
    plugins: catalog.plugins.map((plugin) => {
      const next = update.plugins[plugin.name];
      return {
        ...plugin,
        enabled: next === undefined ? plugin.enabled : next,
        skills: plugin.skills.map(applySkill),
      };
    }),
    skills: catalog.skills.map(applySkill),
  };
}

export type PluginCatalogState = {
  catalog: PluginCatalog | null;
  loading: boolean;
  /** A failed *load*; a failed toggle reverts and speaks through a toast. */
  error: string | null;
  reload: () => void;
  /** Toggle optimistically, then reconcile from what the server returns. */
  setEnabled: (update: PluginEnableUpdate) => void;
};

/**
 * The Plugins library's one piece of state, owned above both views so the list
 * and a bundle's detail are never two catalogs disagreeing about the same
 * switch.
 *
 * Toggling is optimistic and reconciled: the response to `PUT /plugins/enabled`
 * is the whole catalog, so it is taken as the truth rather than merged into a
 * local guess. A failure puts back the catalog the toggle started from — the
 * switch snaps back and says why, instead of leaving the surface claiming a
 * state the server never recorded.
 */
export function usePluginCatalog(apis: PluginsApis): PluginCatalogState {
  const [catalog, setCatalog] = useState<PluginCatalog | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Bumped by every load and every write, so a slow response that has been
  // overtaken — by a reload, an unmount, or a later toggle — is dropped rather
  // than reinstating a catalog the reader has already moved past.
  const generationRef = useRef(0);

  const reload = useCallback(() => {
    const generation = ++generationRef.current;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const loaded = await apis.list();
        if (generation !== generationRef.current) return;
        setCatalog(loaded);
      } catch (caught) {
        if (generation !== generationRef.current) return;
        setError(friendlyPluginError(caught, "Could not load your plugins."));
      } finally {
        if (generation === generationRef.current) setLoading(false);
      }
    })();
  }, [apis]);

  useEffect(() => {
    reload();
    return () => {
      generationRef.current += 1;
    };
  }, [reload]);

  const setEnabled = useCallback(
    (update: PluginEnableUpdate) => {
      const previous = catalog;
      if (!previous) return;
      const generation = ++generationRef.current;
      setCatalog(applyEnableUpdate(previous, update));
      void (async () => {
        try {
          const fresh = await apis.setEnabled(update);
          if (generation !== generationRef.current) return;
          setCatalog(fresh);
        } catch (caught) {
          if (generation !== generationRef.current) return;
          setCatalog(previous);
          toast.error(
            friendlyPluginError(caught, "Could not save that change."),
          );
        }
      })();
    },
    [apis, catalog],
  );

  return { catalog, loading, error, reload, setEnabled };
}

export function friendlyPluginError(error: unknown, fallback: string): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : fallback;
}
