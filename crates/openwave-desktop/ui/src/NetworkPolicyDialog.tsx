import { useState } from "react";
import {
  Check,
  Globe2,
  Package,
  ShieldOff,
  SlidersHorizontal,
} from "lucide-react";

import type { NetworkPolicy } from "./api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Textarea } from "@/components/ui/textarea";

const OPTIONS = [
  {
    mode: "off",
    label: "Network off",
    description: "Commands cannot make outbound connections.",
    icon: ShieldOff,
  },
  {
    mode: "package_managers",
    label: "Package installs",
    description: "Only curated package registries are reachable over HTTPS.",
    icon: Package,
  },
  {
    mode: "allowed_hosts",
    label: "Custom hosts",
    description: "Reach only the exact hosts you list below.",
    icon: SlidersHorizontal,
  },
  {
    mode: "open",
    label: "Open internet",
    description: "Reach public internet destinations; local networks stay blocked.",
    icon: Globe2,
  },
] as const;

function optionFor(policy: NetworkPolicy) {
  return OPTIONS.find((option) => option.mode === policy.mode) ?? OPTIONS[0];
}

/** The policy's short name, for the menu row that opens this. */
export function networkPolicyLabel(policy: NetworkPolicy): string {
  return optionFor(policy).label;
}

export function networkPolicyIcon(policy: NetworkPolicy) {
  return optionFor(policy).icon;
}

/**
 * The per-chat network policy, in a dialog rather than a control of its own on
 * the message bar. It is set once for a workspace and then left alone, so it
 * lives behind the composer's tools menu with the other setup actions; the
 * dialog also gives the custom-host list room a popover never had.
 */
export function NetworkPolicyDialog({
  open,
  onOpenChange,
  value,
  disabled,
  onChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  value: NetworkPolicy;
  disabled?: boolean;
  onChange: (policy: NetworkPolicy) => void | Promise<void>;
}) {
  // Seeded from the stored policy each time the dialog opens, so an abandoned
  // edit does not survive into the next visit.
  const [hosts, setHosts] = useState("");
  const [includePackages, setIncludePackages] = useState(false);

  function openAndHydrate(next: boolean) {
    if (next && value.mode === "allowed_hosts") {
      setHosts(value.allowed_hosts.join("\n"));
      setIncludePackages(value.package_managers);
    }
    onOpenChange(next);
  }

  function select(policy: NetworkPolicy) {
    void onChange(policy);
    onOpenChange(false);
  }

  function saveCustom() {
    const allowedHosts = [
      ...new Set(
        hosts
          .split(/[\s,]+/)
          .map((host) => host.trim())
          .filter(Boolean),
      ),
    ];
    select({
      mode: "allowed_hosts",
      allowed_hosts: allowedHosts,
      package_managers: includePackages,
    });
  }

  return (
    <Dialog open={open} onOpenChange={openAndHydrate}>
      <DialogContent className="max-w-md space-y-3">
        <DialogHeader>
          <DialogTitle>Code execution network</DialogTitle>
          <DialogDescription>
            This applies only to this conversation workspace.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-1">
          {OPTIONS.filter((option) => option.mode !== "allowed_hosts").map(
            (option) => {
              const selected = value.mode === option.mode;
              const Icon = option.icon;
              return (
                <button
                  key={option.mode}
                  type="button"
                  className="flex w-full items-start gap-2 rounded-md p-2 text-left hover:bg-muted"
                  disabled={disabled}
                  onClick={() => select({ mode: option.mode })}
                >
                  <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1">
                    <span className="block text-sm font-medium">{option.label}</span>
                    <span className="block text-xs text-muted-foreground">
                      {option.description}
                    </span>
                  </span>
                  {selected && <Check className="mt-0.5 size-4" />}
                </button>
              );
            },
          )}
        </div>

        <div className="space-y-2 border-t pt-3">
          <div className="flex items-center gap-2">
            <SlidersHorizontal className="size-4 text-muted-foreground" />
            <span className="text-sm font-medium">Custom hosts</span>
            {value.mode === "allowed_hosts" && (
              <Check className="ml-auto size-4" />
            )}
          </div>
          <Textarea
            aria-label="Allowed network hosts"
            value={hosts}
            disabled={disabled}
            onChange={(event) => setHosts(event.target.value)}
            placeholder={"api.example.com\nfiles.example.com"}
            className="min-h-20 font-mono text-xs"
          />
          <label className="flex items-center gap-2 text-xs">
            <Checkbox
              checked={includePackages}
              disabled={disabled}
              onCheckedChange={(checked) => setIncludePackages(checked === true)}
            />
            Also allow package registries
          </label>
          <Button
            type="button"
            size="sm"
            className="w-full"
            disabled={disabled || (!hosts.trim() && !includePackages)}
            onClick={saveCustom}
          >
            Use custom policy
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
