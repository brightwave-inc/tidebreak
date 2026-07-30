import { useState } from "react";
import {
  Check,
  ChevronDown,
  Globe2,
  Package,
  ShieldOff,
  SlidersHorizontal,
} from "lucide-react";

import type { NetworkPolicy } from "./api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
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

export function NetworkPolicyMenu({
  value,
  disabled,
  onChange,
}: {
  value: NetworkPolicy;
  disabled?: boolean;
  onChange: (policy: NetworkPolicy) => void | Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [hosts, setHosts] = useState("");
  const [includePackages, setIncludePackages] = useState(false);
  const current = optionFor(value);
  const CurrentIcon = current.icon;

  function setOpenAndHydrate(next: boolean) {
    if (next && value.mode === "allowed_hosts") {
      setHosts(value.allowed_hosts.join("\n"));
      setIncludePackages(value.package_managers);
    }
    setOpen(next);
  }

  function select(policy: NetworkPolicy) {
    void onChange(policy);
    setOpen(false);
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
    <Popover open={open} onOpenChange={setOpenAndHydrate}>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          className="h-8 gap-1.5"
          disabled={disabled}
          aria-label={`Network: ${current.label}`}
        >
          <CurrentIcon className="size-4 text-muted-foreground" />
          {current.label}
          <ChevronDown className="size-4 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" side="top" className="w-80 space-y-3">
        <div>
          <p className="font-medium">Code execution network</p>
          <p className="text-xs text-muted-foreground">
            This applies only to this conversation workspace.
          </p>
        </div>

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
      </PopoverContent>
    </Popover>
  );
}
