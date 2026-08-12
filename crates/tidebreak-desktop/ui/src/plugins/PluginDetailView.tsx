import { useState } from "react";
import { ChevronLeft } from "lucide-react";
import type { ReactNode } from "react";

import type { PluginSkillInfo } from "@/api";
import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { PluginGlyph, SkillGlyph } from "./PluginGlyph";
import type { PluginsApis } from "./pluginsApis";
import { capabilityLabel, categoryLabel } from "./pluginVocabulary";
import { SkillDialog } from "./SkillDialog";
import type { PluginCatalogState } from "./usePluginCatalog";

/**
 * One bundle's page: identity up top, its member skills, and the facts the
 * host derives about it.
 *
 * A member's switch is gated while the bundle itself is off, because a member
 * of a disabled bundle cannot run whatever its own flag says. It is gated
 * rather than cleared: the server keeps member flags independently, so the
 * choices made here come back exactly as they were when the bundle does.
 *
 * A skill row opens the skill itself — its full instruction body — in a
 * dialog, so what a skill actually teaches the model is one click away from
 * the switch that turns it on.
 */
export function PluginDetailView({
  pluginId,
  state,
  loadInstructions,
  onBack,
}: {
  pluginId: string;
  state: PluginCatalogState;
  loadInstructions: PluginsApis["instructions"];
  /** Return to the plugins list. */
  onBack: () => void;
}) {
  const { catalog, loading, error, setEnabled } = state;
  const plugin = catalog?.plugins.find((entry) => entry.name === pluginId) ?? null;
  const [openSkill, setOpenSkill] = useState<PluginSkillInfo | null>(null);
  // The dialog re-reads its skill from the fresh catalog, so its switch moves
  // with the toggle round trip instead of freezing at the row that opened it.
  const shownSkill = openSkill
    ? (plugin?.skills.find((skill) => skill.name === openSkill.name) ?? openSkill)
    : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PanelSecondaryHeader showBorder={false} className="pr-1 pl-2">
        <Button variant="ghost" size="icon-sm" onClick={onBack}>
          <ChevronLeft className="size-4" />
          <span className="sr-only">Back to plugins</span>
        </Button>
      </PanelSecondaryHeader>

      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-8 px-6 pt-2 pb-8">
          {error && (
            <p className="text-critical text-sm" role="alert">
              {error}
            </p>
          )}
          {!error && loading && !catalog && (
            <p className="text-muted-foreground text-sm" role="status">
              Loading plugin…
            </p>
          )}
          {!error && catalog && !plugin && (
            <p className="text-muted-foreground text-sm" role="status">
              That plugin is not installed.
            </p>
          )}

          {plugin && (
            <>
              <section className="flex flex-col gap-4" aria-label="About">
                <PluginGlyph
                  pluginName={plugin.name}
                  category={plugin.category}
                  size="lg"
                />
                <div className="flex items-start justify-between gap-4">
                  <div className="flex min-w-0 flex-col gap-1.5">
                    <h1 className="text-2xl font-semibold tracking-tight">
                      {plugin.display_name}
                    </h1>
                    <p className="text-muted-foreground text-sm text-pretty">
                      {plugin.description}
                    </p>
                  </div>
                  <Switch
                    aria-label={`Enable ${plugin.display_name}`}
                    checked={plugin.enabled}
                    onCheckedChange={(enabled) =>
                      setEnabled({ plugins: { [plugin.name]: enabled }, skills: {} })
                    }
                    className="mt-1"
                  />
                </div>
              </section>

              <section className="flex flex-col gap-1" aria-label="Skills">
                <div className="flex items-baseline gap-2 border-b pb-2">
                  <h2 className="text-sm font-semibold">Skills</h2>
                  <span className="text-muted-foreground text-xs tabular-nums">
                    {plugin.skills.length}
                  </span>
                </div>
                {!plugin.enabled && plugin.skills.length > 0 && (
                  <p className="text-muted-foreground pt-1 text-xs">
                    Turn the plugin on to change which of its skills are
                    available. Your choices are kept while it is off.
                  </p>
                )}
                <ul className="flex flex-col pt-1" aria-label="Member skills">
                  {plugin.skills.map((skill) => (
                    <li
                      key={skill.name}
                      className={cn(
                        "hover:bg-muted/60 -mx-2 flex items-center gap-3 rounded-xl px-2 py-2.5 transition-colors",
                        !plugin.enabled && "opacity-80",
                      )}
                    >
                      <button
                        type="button"
                        className="flex min-w-0 flex-1 cursor-pointer items-center gap-3 text-left"
                        onClick={() => setOpenSkill(skill)}
                      >
                        <SkillGlyph size="sm" />
                        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                          <span
                            className={cn(
                              "truncate text-sm font-medium",
                              !plugin.enabled && "text-muted-foreground",
                            )}
                          >
                            {skill.name}
                          </span>
                          <span className="text-muted-foreground line-clamp-2 text-xs">
                            {skill.description}
                          </span>
                        </span>
                      </button>
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
                  <p className="text-muted-foreground pt-1 text-sm">
                    This plugin bundles no skills.
                  </p>
                )}
              </section>

              <section className="flex flex-col gap-1" aria-label="Information">
                <h2 className="border-b pb-2 text-sm font-semibold">Information</h2>
                <dl className="grid grid-cols-[8rem_1fr] gap-x-4 gap-y-2 pt-2 text-sm">
                  <InfoRow label="Category">{categoryLabel(plugin.category)}</InfoRow>
                  <InfoRow label="Capabilities">
                    {plugin.capabilities.length === 0 ? (
                      <span className="text-muted-foreground">None derived</span>
                    ) : (
                      <span className="flex flex-wrap gap-1.5">
                        {plugin.capabilities.map((capability) => (
                          <Badge key={capability} variant="outline" size="sm">
                            {capabilityLabel(capability)}
                          </Badge>
                        ))}
                      </span>
                    )}
                  </InfoRow>
                  <InfoRow label="Skills">{String(plugin.skills.length)}</InfoRow>
                </dl>
              </section>
            </>
          )}
        </div>
      </div>

      <SkillDialog
        skill={shownSkill}
        gated={plugin !== null && !plugin.enabled}
        onOpenChange={(open) => {
          if (!open) setOpenSkill(null);
        }}
        onToggle={(skill, enabled) =>
          setEnabled({ plugins: {}, skills: { [skill.name]: enabled } })
        }
        loadInstructions={loadInstructions}
      />
    </div>
  );
}

function InfoRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </>
  );
}
