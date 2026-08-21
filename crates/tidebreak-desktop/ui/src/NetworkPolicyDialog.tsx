import { useState } from "react";
import { Globe2, Package, ShieldOff, SlidersHorizontal } from "lucide-react";

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
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Textarea } from "@/components/ui/textarea";

const OPTIONS = [
  {
    mode: "off",
    label: "Offline",
    description: "Opt in to blocking outbound network access for this work.",
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
    label: "Internet access",
    description:
      "Reach public internet destinations; local networks stay blocked.",
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
  const [selectedMode, setSelectedMode] = useState<NetworkPolicy["mode"]>(
    value.mode,
  );
  const [hosts, setHosts] = useState(
    value.mode === "allowed_hosts" ? value.allowed_hosts.join("\n") : "",
  );
  const [includePackages, setIncludePackages] = useState(
    value.mode === "allowed_hosts" && value.package_managers,
  );
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const controlsDisabled = disabled || saving;

  function openAndHydrate(next: boolean) {
    if (!next && saving) return;
    if (!next) setSaveError(null);
    if (next) {
      setSelectedMode(value.mode);
      setHosts(
        value.mode === "allowed_hosts" ? value.allowed_hosts.join("\n") : "",
      );
      setIncludePackages(
        value.mode === "allowed_hosts" && value.package_managers,
      );
    }
    onOpenChange(next);
  }

  async function select(policy: NetworkPolicy) {
    if (controlsDisabled) return;
    setSaving(true);
    setSaveError(null);
    try {
      await onChange(policy);
      onOpenChange(false);
    } catch (caught) {
      const message = String(caught)
        .replace(/^Error:\s*/, "")
        .trim();
      setSaveError(message || "Could not update the network policy.");
    } finally {
      setSaving(false);
    }
  }

  function saveSelection() {
    if (selectedMode !== "allowed_hosts") {
      void select({ mode: selectedMode });
      return;
    }
    const allowedHosts = [
      ...new Set(
        hosts
          .split(/[\s,]+/)
          .map((host) => host.trim())
          .filter(Boolean),
      ),
    ];
    void select({
      mode: "allowed_hosts",
      allowed_hosts: allowedHosts,
      package_managers: includePackages,
    });
  }

  return (
    <Dialog open={open} onOpenChange={openAndHydrate}>
      <DialogContent
        className="max-w-md space-y-3"
        aria-busy={saving}
        withCloseButton={!saving}
      >
        <DialogHeader>
          <DialogTitle>Code execution network</DialogTitle>
          <DialogDescription>
            This applies only to this conversation workspace.
          </DialogDescription>
        </DialogHeader>

        <RadioGroup
          value={selectedMode}
          onValueChange={(mode) =>
            setSelectedMode(mode as NetworkPolicy["mode"])
          }
          aria-label="Network access policy"
          disabled={controlsDisabled}
          className="gap-1"
        >
          {OPTIONS.filter((option) => option.mode !== "allowed_hosts").map(
            (option) => {
              const Icon = option.icon;
              const id = `network-policy-${option.mode}`;
              return (
                <label
                  key={option.mode}
                  htmlFor={id}
                  className="flex w-full cursor-pointer items-start gap-2 rounded-md p-2 text-left hover:bg-muted"
                >
                  <RadioGroupItem
                    id={id}
                    value={option.mode}
                    className="mt-0.5"
                  />
                  <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1">
                    <span className="block text-sm font-medium">
                      {option.label}
                    </span>
                    <span className="block text-xs text-muted-foreground">
                      {option.description}
                    </span>
                  </span>
                </label>
              );
            },
          )}

          <div className="space-y-2 border-t pt-3">
            <label
              htmlFor="network-policy-allowed-hosts"
              className="flex cursor-pointer items-center gap-2"
            >
              <RadioGroupItem
                id="network-policy-allowed-hosts"
                value="allowed_hosts"
              />
              <SlidersHorizontal className="size-4 text-muted-foreground" />
              <span className="text-sm font-medium">Custom hosts</span>
            </label>
            <Textarea
              aria-label="Allowed network hosts"
              value={hosts}
              disabled={controlsDisabled}
              onChange={(event) => {
                setSelectedMode("allowed_hosts");
                setHosts(event.target.value);
              }}
              placeholder={"api.example.com\nfiles.example.com"}
              className="min-h-20 font-mono text-xs"
            />
            <label className="flex items-center gap-2 text-xs">
              <Checkbox
                checked={includePackages}
                disabled={controlsDisabled}
                onCheckedChange={(checked) => {
                  setSelectedMode("allowed_hosts");
                  setIncludePackages(checked === true);
                }}
              />
              Also allow package registries
            </label>
          </div>
        </RadioGroup>
        <Button
          type="button"
          size="sm"
          className="w-full"
          disabled={
            controlsDisabled ||
            (selectedMode === "allowed_hosts" &&
              !hosts.trim() &&
              !includePackages)
          }
          onClick={saveSelection}
        >
          Apply network policy
        </Button>
        {saving && (
          <p className="text-muted-foreground text-sm" role="status">
            Saving network policy…
          </p>
        )}
        {saveError && (
          <p className="text-destructive text-sm" role="alert">
            {saveError}
          </p>
        )}
      </DialogContent>
    </Dialog>
  );
}
