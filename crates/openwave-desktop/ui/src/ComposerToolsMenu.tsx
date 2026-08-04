import { useState } from "react";
import { FolderPlus, LoaderCircle, Paperclip, Plus } from "lucide-react";

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
import { WithTooltip } from "@/components/ui/tooltip";

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

export type ComposerToolsMenuProps = {
  disabled: boolean;
  attachFiles?: { attaching: boolean; onAttach: () => void };
  attachFolder?: { working: boolean; onAttach: () => void };
  network?: ComposerNetwork;
  reasoning?: ComposerReasoning;
};

/**
 * Everything that sets a turn up, behind one button.
 *
 * The message bar used to carry a separate control per action — two attachment
 * buttons, the effort picker, the network picker — which read as a toolbar
 * rather than a place to write. These actions are all things you reach for
 * occasionally and then forget about, so they collapse into one menu and leave
 * the bar to the model, the permission mode, and sending.
 */
export function ComposerToolsMenu({
  disabled,
  attachFiles,
  attachFolder,
  network,
  reasoning,
}: ComposerToolsMenuProps) {
  // The dialog is a sibling of the menu, not a child: selecting the row closes
  // the menu, and a dialog rendered inside the menu's content would be torn
  // down with it before it could open.
  const [networkOpen, setNetworkOpen] = useState(false);

  const hasAttachments = Boolean(attachFiles || attachFolder);
  const hasSettings = Boolean(network || reasoning);
  if (!hasAttachments && !hasSettings) return null;

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
        <DropdownMenuContent align="start" side="top" className="w-60">
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
