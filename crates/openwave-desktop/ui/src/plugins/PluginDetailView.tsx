import { ChevronLeft } from "lucide-react";

import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { capabilityLabel, categoryIcon, categoryLabel } from "./pluginVocabulary";
import type { PluginCatalogState } from "./usePluginCatalog";

/**
 * One bundle, as the panel addressed `plugins.{slug}`: what it is for, what it
 * can do, and which of its skills are on.
 *
 * A member's switch is gated while the bundle itself is off, because a member
 * of a disabled bundle cannot run whatever its own flag says. It is gated
 * rather than cleared: the server keeps member flags independently, so the
 * choices made here come back exactly as they were when the bundle does.
 */
export function PluginDetailView({
  pluginId,
  state,
  onBack,
}: {
  pluginId: string;
  state: PluginCatalogState;
  /** Return to the `plugins` list panel. */
  onBack: () => void;
}) {
  const { catalog, loading, error, setEnabled } = state;
  const plugin = catalog?.plugins.find((entry) => entry.name === pluginId) ?? null;
  const Icon = plugin ? categoryIcon(plugin.category) : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PanelSecondaryHeader showBorder={false} className="pr-1 pl-2">
        <Button variant="ghost" size="icon-sm" onClick={onBack}>
          <ChevronLeft className="size-4" />
          <span className="sr-only">Back to plugins</span>
        </Button>
        <h1 className="min-w-0 truncate text-lg font-medium">
          {plugin?.display_name ?? "Plugin"}
        </h1>
      </PanelSecondaryHeader>

      <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto pt-4 pb-4">
        {error && (
          <p className="text-critical mx-4 text-sm" role="alert">
            {error}
          </p>
        )}
        {!error && loading && !catalog && (
          <p className="text-muted-foreground mx-4 text-sm" role="status">
            Loading plugin…
          </p>
        )}
        {!error && catalog && !plugin && (
          <p className="text-muted-foreground mx-4 text-sm" role="status">
            That plugin is not installed.
          </p>
        )}

        {plugin && (
          <>
            <section className="mx-4 flex flex-col gap-3" aria-label="About">
              <div className="flex items-start gap-3">
                <div className="flex min-w-0 flex-1 flex-col gap-1">
                  <div className="text-muted-foreground flex items-center gap-1.5 text-xs">
                    {Icon && <Icon className="size-3.5 shrink-0" aria-hidden="true" />}
                    <span>{categoryLabel(plugin.category)}</span>
                  </div>
                  <p className="text-sm">{plugin.description}</p>
                </div>
                <Switch
                  aria-label={`Enable ${plugin.display_name}`}
                  checked={plugin.enabled}
                  onCheckedChange={(enabled) =>
                    setEnabled({ plugins: { [plugin.name]: enabled }, skills: {} })
                  }
                />
              </div>

              {plugin.capabilities.length > 0 && (
                <div className="flex flex-wrap gap-1.5">
                  {plugin.capabilities.map((capability) => (
                    <Badge key={capability} variant="outline" size="sm">
                      {capabilityLabel(capability)}
                    </Badge>
                  ))}
                </div>
              )}
            </section>

            <section className="mx-4 flex flex-col gap-2" aria-label="Skills">
              <div className="flex flex-col gap-0.5">
                <h2 className="text-sm font-medium">Skills</h2>
                {!plugin.enabled && (
                  <p className="text-muted-foreground text-xs">
                    Turn the plugin on to change which of its skills are
                    available. Your choices are kept while it is off.
                  </p>
                )}
              </div>
              <ul className="flex flex-col gap-1" aria-label="Member skills">
                {plugin.skills.map((skill) => (
                  <li key={skill.name} className="flex items-center gap-3 rounded-md p-2">
                    <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                      <span
                        className={`truncate text-sm font-medium${
                          plugin.enabled ? "" : " text-muted-foreground"
                        }`}
                      >
                        {skill.name}
                      </span>
                      <span className="text-muted-foreground line-clamp-2 text-xs">
                        {skill.description}
                      </span>
                    </div>
                    <Switch
                      aria-label={`Enable ${skill.name}`}
                      checked={skill.enabled}
                      disabled={!plugin.enabled}
                      onCheckedChange={(enabled) =>
                        setEnabled({ plugins: {}, skills: { [skill.name]: enabled } })
                      }
                    />
                  </li>
                ))}
              </ul>
              {plugin.skills.length === 0 && (
                <p className="text-muted-foreground text-sm">
                  This plugin bundles no skills.
                </p>
              )}
            </section>
          </>
        )}
      </div>
    </div>
  );
}
