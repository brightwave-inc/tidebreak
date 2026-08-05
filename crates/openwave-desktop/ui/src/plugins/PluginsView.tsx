import { ChevronRight, Puzzle } from "lucide-react";
import type { ReactNode } from "react";

import type { PluginInfo, PluginSkillInfo } from "@/api";
import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Switch } from "@/components/ui/switch";
import { capabilityShortLabel, categoryIcon } from "./pluginVocabulary";
import type { PluginCatalogState } from "./usePluginCatalog";

/**
 * The Plugins library, as the panel addressed `plugins`.
 *
 * Install-wide rather than conversation-scoped — a bundle being on is a
 * property of this installation, not of the chat you happened to be in when
 * you switched it — which is why this hangs off the home rail beside the Apps
 * library. Picking a row opens `plugins.{slug}`: the bundle's description, its
 * badges, and its member skills with their own switches.
 *
 * The origin split is the catalog's own: bundles first, then the skills no
 * bundle claims, with anything the user wrote themselves called out as theirs.
 */
export function PluginsView({
  state,
  onOpen,
}: {
  state: PluginCatalogState;
  /** Navigate to the `plugins.{slug}` panel contract. */
  onOpen: (pluginId: string) => void;
}) {
  const { catalog, loading, error, reload, setEnabled } = state;
  const yourSkills = catalog?.skills.filter((skill) => skill.origin === "user") ?? [];
  const otherSkills =
    catalog?.skills.filter((skill) => skill.origin !== "user") ?? [];
  const isEmpty =
    catalog !== null && catalog.plugins.length === 0 && catalog.skills.length === 0;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PanelSecondaryHeader showBorder={false} className="pr-1 pl-4">
        <h1 className="text-lg font-medium">Plugins</h1>
      </PanelSecondaryHeader>

      <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto pt-4 pb-4">
        {error && (
          <div
            className="mx-4 flex shrink-0 items-center justify-between gap-3 rounded-md bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted"
            role="alert"
          >
            <span>{error}</span>
            <Button variant="outline" size="xs" className="shrink-0" onClick={reload}>
              Try again
            </Button>
          </div>
        )}

        {loading && !catalog && (
          <p className="text-muted-foreground px-4 text-sm" role="status">
            Loading your plugins…
          </p>
        )}

        {isEmpty && (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <Puzzle />
              </EmptyMedia>
              <EmptyTitle>No plugins installed</EmptyTitle>
              <EmptyDescription>
                Plugins bundle the skills OpenWave can use in a conversation.
                This installation has none available — the ones that ship with
                the app appear here once code execution is set up.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        )}

        {catalog && catalog.plugins.length > 0 && (
          <Section title="Plugins">
            <ul className="flex flex-col gap-1" aria-label="Plugins">
              {catalog.plugins.map((plugin) => (
                <li key={plugin.name}>
                  <PluginRow
                    plugin={plugin}
                    onOpen={() => onOpen(plugin.name)}
                    onToggle={(enabled) =>
                      setEnabled({ plugins: { [plugin.name]: enabled }, skills: {} })
                    }
                  />
                </li>
              ))}
            </ul>
          </Section>
        )}

        {yourSkills.length > 0 && (
          <Section
            title="Your skills"
            description="Skills you wrote, loaded from your data directory. They stand on their own rather than belonging to a bundle."
          >
            <SkillList skills={yourSkills} setEnabled={setEnabled} label="Your skills" />
          </Section>
        )}

        {otherSkills.length > 0 && (
          <Section
            title="Other skills"
            description="Installed skills that no bundle claims."
          >
            <SkillList skills={otherSkills} setEnabled={setEnabled} label="Other skills" />
          </Section>
        )}
      </div>
    </div>
  );
}

function Section({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <section className="mx-3 flex flex-col gap-2" aria-label={title}>
      <div className="flex flex-col gap-0.5 px-1">
        <h2 className="text-sm font-medium">{title}</h2>
        {description && (
          <p className="text-muted-foreground text-xs">{description}</p>
        )}
      </div>
      {children}
    </section>
  );
}

/**
 * A bundle's row: the whole row opens the detail, and the switch — which is
 * not inside that button — turns the bundle on and off without leaving the
 * list.
 */
function PluginRow({
  plugin,
  onOpen,
  onToggle,
}: {
  plugin: PluginInfo;
  onOpen: () => void;
  onToggle: (enabled: boolean) => void;
}) {
  const Icon = categoryIcon(plugin.category);
  return (
    <div className="hover:bg-muted flex items-center gap-3 rounded-md p-2 transition-colors">
      <button
        type="button"
        className="flex min-w-0 flex-1 items-start gap-3 text-left"
        onClick={onOpen}
      >
        <Icon className="text-muted-foreground mt-0.5 size-4 shrink-0" aria-hidden="true" />
        <span className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="flex min-w-0 items-center gap-1.5">
            <span className="truncate text-sm font-medium">{plugin.display_name}</span>
            <ChevronRight
              className="text-muted-foreground size-3.5 shrink-0"
              aria-hidden="true"
            />
          </span>
          <span className="text-muted-foreground line-clamp-2 text-xs">
            {plugin.description}
          </span>
          {plugin.capabilities.length > 0 && (
            <span className="flex flex-wrap gap-1 pt-0.5">
              {plugin.capabilities.map((capability) => (
                <Badge key={capability} variant="outline" size="sm">
                  {capabilityShortLabel(capability)}
                </Badge>
              ))}
            </span>
          )}
        </span>
      </button>
      <Switch
        aria-label={`Enable ${plugin.display_name}`}
        checked={plugin.enabled}
        onCheckedChange={onToggle}
      />
    </div>
  );
}

function SkillList({
  skills,
  setEnabled,
  label,
}: {
  skills: PluginSkillInfo[];
  setEnabled: PluginCatalogState["setEnabled"];
  label: string;
}) {
  return (
    <ul className="flex flex-col gap-1" aria-label={label}>
      {skills.map((skill) => (
        <li
          key={skill.name}
          className="flex items-center gap-3 rounded-md p-2 transition-colors"
        >
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <span className="truncate text-sm font-medium">{skill.name}</span>
            <span className="text-muted-foreground line-clamp-2 text-xs">
              {skill.description}
            </span>
          </div>
          <Switch
            aria-label={`Enable ${skill.name}`}
            checked={skill.enabled}
            onCheckedChange={(enabled) =>
              setEnabled({ plugins: {}, skills: { [skill.name]: enabled } })
            }
          />
        </li>
      ))}
    </ul>
  );
}
