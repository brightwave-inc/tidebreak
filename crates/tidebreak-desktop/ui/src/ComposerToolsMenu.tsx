import { useState } from "react";
import {
  Brain,
  FolderPlus,
  LoaderCircle,
  Package,
  Paperclip,
  Plus,
} from "lucide-react";

import type { NetworkPolicy, ReasoningEffort } from "./api";
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
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Switch } from "@/components/ui/switch";
import { WithTooltip } from "@/components/ui/tooltip";
import { useGuidedMenu } from "./FirstTaskWalkthrough";

export type ComposerNetwork = {
  value: NetworkPolicy;
  disabled?: boolean;
  onChange: (policy: NetworkPolicy) => void | Promise<void>;
};

export type ComposerMemoryIncognito = {
  value: boolean;
  disabled?: boolean;
  onChange: (memoryIncognito: boolean) => void | Promise<void>;
};

export type ComposerReasoning = {
  levels: readonly ReasoningEffort[];
  value: ReasoningEffort | null;
  disabled?: boolean;
  onChange: (effort: ReasoningEffort | null) => void | Promise<void>;
};

/**
 * The way into the plugin library from the menu.
 *
 * Presentational: the menu reports that the row was chosen. What the library
 * holds, and what picking one of its rows does, belongs to the composer that
 * owns the draft and the message's invocations.
 */
export type ComposerPluginsEntry = {
  onOpen: () => void;
};

export type ComposerToolsMenuProps = {
  disabled: boolean;
  attachFiles?: { attaching: boolean; onAttach: () => void };
  attachFolder?: { working: boolean; onAttach: () => void };
  network?: ComposerNetwork;
  reasoning?: ComposerReasoning;
  memoryIncognito?: ComposerMemoryIncognito;
  plugins?: ComposerPluginsEntry;
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
 * The library sits at the bottom as a single row rather than a bundle per line:
 * it is the same list `/` reaches, and duplicating it here left two pickers
 * that looked alike and behaved differently. Managing the library still lives
 * on the Plugins page.
 */
export function ComposerToolsMenu({
  disabled,
  attachFiles,
  attachFolder,
  network,
  reasoning,
  memoryIncognito,
  plugins,
}: ComposerToolsMenuProps) {
  // The dialog is a sibling of the menu, not a child: selecting the row closes
  // the menu, and a dialog rendered inside the menu's content would be torn
  // down with it before it could open.
  const [networkOpen, setNetworkOpen] = useState(false);
  const guided = useGuidedMenu("tools");

  const hasAttachments = Boolean(attachFiles || attachFolder);
  const hasSettings = Boolean(network || reasoning || memoryIncognito);
  // A catalog that is empty or never loaded simply has no row. The menu is not
  // the place to report that the plugin library could not be read.
  const hasPlugins = Boolean(plugins);
  if (!hasAttachments && !hasSettings && !hasPlugins) return null;

  const NetworkIcon = network ? networkPolicyIcon(network.value) : null;

  return (
    <>
      <DropdownMenu
        open={guided.open}
        modal={guided.modal}
        onOpenChange={guided.onOpenChange}
      >
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
          className="w-60"
          data-first-task-target="tools-menu"
          onEscapeKeyDown={guided.onEscapeKeyDown}
        >
          {attachFiles && (
            <DropdownMenuItem
              disabled={disabled || attachFiles.attaching}
              onSelect={attachFiles.onAttach}
              data-first-task-target="attach-files"
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
              data-first-task-target="attach-folder"
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
              data-first-task-target="network"
              onSelect={() => {
                // The walkthrough holds this menu open, so a second overlay
                // would sit under the spotlight and trap focus.
                if (guided.guided) return;
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
          {memoryIncognito && (
            <DropdownMenuItem
              disabled={disabled || memoryIncognito.disabled}
              onSelect={(event) => {
                // The row toggles in place; closing the menu would hide the
                // switch state the flip just changed.
                event.preventDefault();
                void memoryIncognito.onChange(!memoryIncognito.value);
              }}
            >
              <Brain className="size-4 text-muted-foreground" />
              <span className="min-w-0 flex-1">
                <span className="block">Memory incognito</span>
                <span className="block text-xs text-muted-foreground">
                  Keep this chat out of memory: nothing is injected and nothing
                  is captured.
                </span>
              </span>
              <Switch
                checked={memoryIncognito.value}
                disabled={disabled || memoryIncognito.disabled}
                aria-label="Memory incognito"
                // The menu row owns the click; the switch only shows state.
                className="pointer-events-none"
                tabIndex={-1}
              />
            </DropdownMenuItem>
          )}

          {hasPlugins && plugins && (
            <>
              {(hasAttachments || hasSettings) && <DropdownMenuSeparator />}
              <DropdownMenuItem
                disabled={disabled}
                onSelect={() => {
                  // Opened once the menu has gone: the panel takes focus for
                  // its search field, which the closing menu would take back.
                  window.requestAnimationFrame(plugins.onOpen);
                }}
              >
                <Package
                  className="size-4 text-muted-foreground"
                  aria-hidden="true"
                />
                <span>Plugins</span>
              </DropdownMenuItem>
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
