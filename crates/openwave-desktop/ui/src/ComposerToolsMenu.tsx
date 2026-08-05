import { useState } from "react";
import { FolderPlus, LoaderCircle, Paperclip, Plus } from "lucide-react";

import type { NetworkPolicy, PluginInfo, ReasoningEffort } from "./api";
import { ReasoningEffortSubMenu } from "./ModelMenu";
import {
  NetworkPolicyDialog,
  networkPolicyIcon,
  networkPolicyLabel,
} from "./NetworkPolicyDialog";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { categoryIcon } from "@/plugins/pluginVocabulary";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

export type ComposerNetwork = {
  value: NetworkPolicy;
  disabled?: boolean;
  onChange: (policy: NetworkPolicy) => void | Promise<void>;
};

export type ComposerReasoning = {
  levels: readonly ReasoningEffort[];
  value: ReasoningEffort | null;
  disabled?: boolean;
  onChange: (effort: ReasoningEffort | null) => void | Promise<void>;
};

/**
 * The installed bundles, offered as something to reach for on this message.
 *
 * Presentational: the menu lists what it is given and reports the pick. Turning
 * the bundle on and saying so in the draft belong to whoever owns the catalog
 * and the draft.
 */
export type ComposerPlugins = {
  items: readonly PluginInfo[];
  onSelect: (plugin: PluginInfo) => void;
};

export type ComposerToolsMenuProps = {
  disabled: boolean;
  attachFiles?: { attaching: boolean; onAttach: () => void };
  attachFolder?: { working: boolean; onAttach: () => void };
  network?: ComposerNetwork;
  reasoning?: ComposerReasoning;
  plugins?: ComposerPlugins;
};

/**
 * Everything that sets a turn up, behind one button.
 *
 * The message bar used to carry a separate control per action — two attachment
 * buttons, the effort picker, the network picker — which read as a toolbar
 * rather than a place to write. These actions are all things you reach for
 * occasionally and then forget about, so they collapse into one menu and leave
 * the bar to the model, the permission mode, and sending.
 *
 * Installed plugins sit at the bottom for the same reason: picking one is
 * preparation for this message, not a place to manage the library — that stays
 * on the Plugins page.
 */
export function ComposerToolsMenu({
  disabled,
  attachFiles,
  attachFolder,
  network,
  reasoning,
  plugins,
}: ComposerToolsMenuProps) {
  // The dialog is a sibling of the menu, not a child: selecting the row closes
  // the menu, and a dialog rendered inside the menu's content would be torn
  // down with it before it could open.
  const [networkOpen, setNetworkOpen] = useState(false);

  const hasAttachments = Boolean(attachFiles || attachFolder);
  const hasSettings = Boolean(network || reasoning);
  // A catalog that is empty or never loaded simply has no section. The menu is
  // not the place to report that the plugin library could not be read.
  const pluginItems = plugins?.items ?? [];
  const hasPlugins = pluginItems.length > 0;
  if (!hasAttachments && !hasSettings && !hasPlugins) return null;

  const NetworkIcon = network ? networkPolicyIcon(network.value) : null;

  return (
    <>
      <DropdownMenu>
        <WithTooltip label="Tools">
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon-8"
              aria-label="Tools"
              disabled={disabled}
            >
              <Plus size={16} />
            </Button>
          </DropdownMenuTrigger>
        </WithTooltip>
        <DropdownMenuContent
          align="start"
          side="top"
          // Plugin rows carry a description under the name, so the menu widens
          // to give one a readable line rather than truncating it to nothing.
          className={cn(hasPlugins ? "w-72" : "w-60")}
        >
          {attachFiles && (
            <DropdownMenuItem
              disabled={disabled || attachFiles.attaching}
              onSelect={attachFiles.onAttach}
            >
              {attachFiles.attaching ? (
                <LoaderCircle className="size-4 animate-spin" />
              ) : (
                <Paperclip className="size-4" />
              )}
              {attachFiles.attaching ? "Attaching files…" : "Attach files"}
            </DropdownMenuItem>
          )}
          {attachFolder && (
            <DropdownMenuItem
              disabled={disabled || attachFolder.working}
              onSelect={attachFolder.onAttach}
            >
              {attachFolder.working ? (
                <LoaderCircle className="size-4 animate-spin" />
              ) : (
                <FolderPlus className="size-4" />
              )}
              {attachFolder.working ? "Updating folders…" : "Attach folder"}
            </DropdownMenuItem>
          )}

          {hasAttachments && hasSettings && <DropdownMenuSeparator />}

          {reasoning && reasoning.levels.length > 0 && (
            <ReasoningEffortSubMenu
              levels={reasoning.levels}
              value={reasoning.value}
              disabled={disabled || reasoning.disabled}
              onChange={reasoning.onChange}
            />
          )}
          {network && NetworkIcon && (
            <DropdownMenuItem
              disabled={disabled || network.disabled}
              onSelect={() => {
                // Opened once the menu has actually gone. Raising the dialog in
                // the same commit leaves two overlays each claiming focus and
                // hiding the other from assistive tech.
                window.requestAnimationFrame(() => setNetworkOpen(true));
              }}
            >
              <NetworkIcon className="size-4 text-muted-foreground" />
              <span>Network</span>
              <span className="text-muted-foreground flex-1 text-right text-xs">
                {networkPolicyLabel(network.value)}
              </span>
            </DropdownMenuItem>
          )}

          {hasPlugins && plugins && (
            <>
              {(hasAttachments || hasSettings) && <DropdownMenuSeparator />}
              <p className="text-muted-foreground px-2 py-1.5 text-xs font-medium">
                Plugins
              </p>
              <DropdownMenuGroup aria-label="Plugins">
                {pluginItems.map((plugin) => {
                  const Icon = categoryIcon(plugin.category);
                  return (
                    <DropdownMenuItem
                      key={plugin.name}
                      className="items-start"
                      disabled={disabled}
                      onSelect={() => plugins.onSelect(plugin)}
                    >
                      <Icon
                        className="text-muted-foreground mt-0.5 size-4 shrink-0"
                        aria-hidden="true"
                      />
                      <span className="flex min-w-0 flex-col">
                        <span className="truncate">{plugin.display_name}</span>
                        <span className="text-muted-foreground truncate text-xs">
                          {plugin.description}
                        </span>
                      </span>
                    </DropdownMenuItem>
                  );
                })}
              </DropdownMenuGroup>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      {network && (
        <NetworkPolicyDialog
          open={networkOpen}
          onOpenChange={setNetworkOpen}
          value={network.value}
          disabled={network.disabled}
          onChange={network.onChange}
        />
      )}
    </>
  );
}
