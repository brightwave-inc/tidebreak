import { RadioGroup, RadioGroupItem } from "tidebreak-desktop-ui";
import { Globe, Lock, SlidersHorizontal } from "lucide-react";

export function NetworkPolicy() {
  return (
    <RadioGroup
      defaultValue="sandboxed"
      aria-label="Network access policy"
      className="gap-1"
      style={{ maxWidth: 420 }}
    >
      <label
        htmlFor="policy-open"
        className="flex w-full cursor-pointer items-start gap-2 rounded-md p-2 text-left"
      >
        <RadioGroupItem id="policy-open" value="open" className="mt-0.5" />
        <Globe className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium">Full network access</span>
          <span className="block text-xs text-muted-foreground">
            The agent can reach any host. Use for trusted repos only.
          </span>
        </span>
      </label>
      <label
        htmlFor="policy-sandboxed"
        className="flex w-full cursor-pointer items-start gap-2 rounded-md p-2 text-left"
      >
        <RadioGroupItem id="policy-sandboxed" value="sandboxed" className="mt-0.5" />
        <Lock className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium">Sandboxed</span>
          <span className="block text-xs text-muted-foreground">
            Package registries and version control only.
          </span>
        </span>
      </label>
      <label
        htmlFor="policy-custom"
        className="flex w-full cursor-pointer items-start gap-2 rounded-md p-2 text-left"
      >
        <RadioGroupItem id="policy-custom" value="custom" className="mt-0.5" />
        <SlidersHorizontal className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium">Custom hosts</span>
          <span className="block text-xs text-muted-foreground">
            Allow an explicit list of hosts, one per line.
          </span>
        </span>
      </label>
    </RadioGroup>
  );
}

export function CompactChoice() {
  return (
    <RadioGroup defaultValue="squash" aria-label="Merge strategy">
      <label htmlFor="merge-squash" className="flex cursor-pointer items-center gap-2 text-sm">
        <RadioGroupItem id="merge-squash" value="squash" />
        Squash and merge
      </label>
      <label htmlFor="merge-rebase" className="flex cursor-pointer items-center gap-2 text-sm">
        <RadioGroupItem id="merge-rebase" value="rebase" />
        Rebase and merge
      </label>
      <label
        htmlFor="merge-commit"
        className="flex items-center gap-2 text-sm text-muted-foreground"
      >
        <RadioGroupItem id="merge-commit" value="commit" disabled />
        Merge commit (disabled by branch protection)
      </label>
    </RadioGroup>
  );
}
