import { Puzzle } from "lucide-react";
import { useMemo, useState, type ReactNode } from "react";

import type { PluginInfo, PluginSkillInfo } from "@/api";
import { SearchInput } from "@/components/SearchInput";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { PluginGlyph, SkillGlyph } from "./PluginGlyph";
import type { PluginsApis } from "./pluginsApis";
import { SkillDialog } from "./SkillDialog";
import {
  hostToolProvisioningLabel,
  useHostToolProvisioning,
} from "./useHostToolProvisioning";
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
 * Laid out as a scannable instrument shelf: a centered title and search, then
 * a two-column grid of tiles. The origin split is the catalog's own: bundles
 * first, then the skills no bundle claims, with anything the user wrote
 * themselves called out as theirs.
 */
export function PluginsView({
  state,
  loadInstructions,
  onOpen,
}: {
  state: PluginCatalogState;
  loadInstructions: PluginsApis["instructions"];
  /** Navigate to the bundle's own page. */
  onOpen: (pluginId: string) => void;
}) {
  const { catalog, loading, error, reload, setEnabled } = state;
  const provisioning = useHostToolProvisioning();
  const [query, setQuery] = useState("");
  const [openSkill, setOpenSkill] = useState<PluginSkillInfo | null>(null);

  const filtered = useMemo(() => filterCatalog(catalog, query), [catalog, query]);
  const yourPlugins = filtered?.plugins.filter((plugin) => plugin.origin === "user") ?? [];
  const otherPlugins =
    filtered?.plugins.filter((plugin) => plugin.origin !== "user") ?? [];
  const yourSkills = filtered?.skills.filter((skill) => skill.origin === "user") ?? [];
  const otherSkills =
    filtered?.skills.filter((skill) => skill.origin !== "user") ?? [];
  const isEmpty =
    catalog !== null && catalog.plugins.length === 0 && catalog.skills.length === 0;
  const noMatches =
    catalog !== null &&
    !isEmpty &&
    otherPlugins.length === 0 &&
    yourPlugins.length === 0 &&
    yourSkills.length === 0 &&
    otherSkills.length === 0;
  // The dialog re-reads its skill from the fresh catalog, so its switch moves
  // with the toggle round trip instead of freezing at the row that opened it.
  const shownSkill = openSkill
    ? (catalog?.skills.find((skill) => skill.name === openSkill.name) ?? openSkill)
    : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-8 px-6 pt-10 pb-12">
          <header className="flex flex-col items-center gap-4 text-center">
            <div className="flex flex-col gap-1.5">
              <h1 className="text-2xl font-semibold tracking-tight">Plugins</h1>
              <p className="text-muted-foreground max-w-md text-sm text-pretty">
                Skills the agent can use when you turn them on.
              </p>
            </div>
            {catalog && !isEmpty && (
              <SearchInput
                value={query}
                onValueChange={setQuery}
                placeholder="Search plugins and skills…"
                aria-label="Search plugins and skills"
                className="w-full max-w-md rounded-full"
              />
            )}
          </header>

          {provisioning && (
            <p className="text-muted-foreground -mt-4 text-center text-xs" role="status">
              {hostToolProvisioningLabel(provisioning)}
            </p>
          )}

          {error && (
            <div
              className="flex shrink-0 items-center justify-between gap-3 rounded-lg bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted"
              role="alert"
            >
              <span>{error}</span>
              <Button variant="outline" size="xs" className="shrink-0" onClick={reload}>
                Try again
              </Button>
            </div>
          )}

          {loading && !catalog && (
            <p className="text-muted-foreground text-center text-sm" role="status">
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

          {noMatches && (
            <p className="text-muted-foreground text-center text-sm" role="status">
              Nothing matches “{query.trim()}”.
            </p>
          )}

          {otherPlugins.length > 0 && (
            <Section title="Plugins">
              <PluginGrid
                plugins={otherPlugins}
                label="Plugins"
                onOpen={onOpen}
                setEnabled={setEnabled}
              />
            </Section>
          )}

          {yourPlugins.length > 0 && (
            <Section
              title="Your plugins"
              description="Plugins you wrote, loaded from your data directory."
            >
              <PluginGrid
                plugins={yourPlugins}
                label="Your plugins"
                onOpen={onOpen}
                setEnabled={setEnabled}
              />
            </Section>
          )}

          {yourSkills.length > 0 && (
            <Section
              title="Your skills"
              description="Skills you wrote, loaded from your data directory. They stand on their own rather than belonging to a bundle."
            >
              <SkillGrid
                skills={yourSkills}
                setEnabled={setEnabled}
                label="Your skills"
                onOpenSkill={setOpenSkill}
              />
            </Section>
          )}

          {otherSkills.length > 0 && (
            <Section
              title="Other skills"
              description="Installed skills that no bundle claims."
            >
              <SkillGrid
                skills={otherSkills}
                setEnabled={setEnabled}
                label="Other skills"
                onOpenSkill={setOpenSkill}
              />
            </Section>
          )}
        </div>
      </div>

      <SkillDialog
        skill={shownSkill}
        gated={false}
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
    <section className="flex flex-col gap-3" aria-label={title}>
      <div className="flex flex-col gap-0.5 px-1">
        <h2 className="text-muted-foreground text-xs font-medium tracking-wide uppercase">
          {title}
        </h2>
        {description && (
          <p className="text-muted-foreground text-xs">{description}</p>
        )}
      </div>
      {children}
    </section>
  );
}

function PluginGrid({
  plugins,
  label,
  onOpen,
  setEnabled,
}: {
  plugins: PluginInfo[];
  label: string;
  onOpen: (pluginId: string) => void;
  setEnabled: PluginCatalogState["setEnabled"];
}) {
  return (
    <ul
      className="grid grid-cols-1 gap-1 sm:grid-cols-2 sm:gap-x-3 sm:gap-y-1"
      aria-label={label}
    >
      {plugins.map((plugin) => (
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
  );
}

/**
 * A bundle's tile: the whole row opens the detail, and the switch — which is
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
  return (
    <div
      className={cn(
        "hover:bg-muted/70 flex items-center gap-3 rounded-xl px-2.5 py-2.5 transition-colors",
        !plugin.enabled && "opacity-80",
      )}
    >
      <button
        type="button"
        className="flex min-w-0 flex-1 items-center gap-3 text-left"
        onClick={onOpen}
      >
        <PluginGlyph pluginName={plugin.name} category={plugin.category} size="md" />
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="truncate text-sm font-medium">{plugin.display_name}</span>
          <span className="text-muted-foreground line-clamp-1 text-xs leading-snug">
            {plugin.description}
          </span>
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

function SkillGrid({
  skills,
  setEnabled,
  label,
  onOpenSkill,
}: {
  skills: PluginSkillInfo[];
  setEnabled: PluginCatalogState["setEnabled"];
  label: string;
  onOpenSkill: (skill: PluginSkillInfo) => void;
}) {
  return (
    <ul
      className="grid grid-cols-1 gap-1 sm:grid-cols-2 sm:gap-x-3 sm:gap-y-1"
      aria-label={label}
    >
      {skills.map((skill) => (
        <li
          key={skill.name}
          className={cn(
            "hover:bg-muted/70 flex items-center gap-3 rounded-xl px-2.5 py-2.5 transition-colors",
            !skill.enabled && "opacity-80",
          )}
        >
          <button
            type="button"
            className="flex min-w-0 flex-1 cursor-pointer items-center gap-3 text-left"
            onClick={() => onOpenSkill(skill)}
          >
            <SkillGlyph size="md" />
            <span className="flex min-w-0 flex-1 flex-col gap-0.5">
              <span className="truncate text-sm font-medium">{skill.name}</span>
              <span className="text-muted-foreground line-clamp-1 text-xs leading-snug">
                {skill.description}
              </span>
            </span>
          </button>
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

function filterCatalog(
  catalog: PluginCatalogState["catalog"],
  query: string,
): PluginCatalogState["catalog"] {
  if (!catalog) return null;
  const needle = query.trim().toLowerCase();
  if (!needle) return catalog;

  const plugins = catalog.plugins.filter(
    (plugin) =>
      plugin.display_name.toLowerCase().includes(needle) ||
      plugin.name.toLowerCase().includes(needle) ||
      plugin.description.toLowerCase().includes(needle) ||
      plugin.skills.some(
        (skill) =>
          skill.name.toLowerCase().includes(needle) ||
          skill.description.toLowerCase().includes(needle),
      ),
  );
  const skills = catalog.skills.filter(
    (skill) =>
      skill.name.toLowerCase().includes(needle) ||
      skill.description.toLowerCase().includes(needle),
  );
  return { ...catalog, plugins, skills };
}
