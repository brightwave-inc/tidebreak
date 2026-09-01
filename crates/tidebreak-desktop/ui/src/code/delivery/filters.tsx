import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  codeDeliveryRepositoryKey,
  type CodeDeliveryAuthor,
  type CodeDeliveryPrViewFilters,
  type CodeDeliveryRunViewFilters,
  useCodeDeliveryStore,
} from "../CodeDeliveryStore";
import type {
  CodeDeliveryRunKind,
  CodeGitHubRepositoryRef,
} from "../../api/types";
import { Filter } from "lucide-react";
import { GithubAvatar } from "../GithubAvatar";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { commaList, humanize, toggleValue } from "./helpers";
import { useMemo, useState } from "react";

export function PullRequestFilters({
  repositories,
  filters,
  onChange,
}: {
  repositories: CodeGitHubRepositoryRef[];
  filters: CodeDeliveryPrViewFilters;
  onChange: (filters: CodeDeliveryPrViewFilters) => void;
}) {
  const count =
    filters.repositoryKeys.length +
    filters.states.length +
    filters.reviewStates.length +
    filters.checkStates.length +
    filters.authors.length +
    Number(filters.attentionOnly) +
    Number(filters.readyOnly) +
    Number(filters.tidebreakLinked !== undefined);
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button type="button" size="sm" variant="outline">
          <Filter />
          Filters
          {count > 0 && (
            <span className="rounded-full bg-primary px-1.5 text-2xs text-primary-foreground">
              {count}
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        className="w-[22rem] max-w-[calc(100vw-24px)] p-3"
      >
        <FilterSection title="Repositories">
          <RepositoryCheckboxes
            repositories={repositories}
            selected={filters.repositoryKeys}
            onChange={(repositoryKeys) =>
              onChange({ ...filters, repositoryKeys })
            }
          />
        </FilterSection>
        <FilterSection title="State">
          <CheckboxOptions
            options={["open", "closed", "merged"]}
            selected={filters.states}
            onChange={(states) => onChange({ ...filters, states })}
          />
        </FilterSection>
        <FilterSection title="Review">
          <CheckboxOptions
            options={["approved", "changes_requested", "review_required"]}
            selected={filters.reviewStates}
            onChange={(reviewStates) => onChange({ ...filters, reviewStates })}
          />
        </FilterSection>
        <FilterSection title="Checks">
          <CheckboxOptions
            options={["pass", "pending", "fail"]}
            selected={filters.checkStates}
            onChange={(checkStates) => onChange({ ...filters, checkStates })}
          />
        </FilterSection>
        <FilterSection title="Authors">
          <AuthorFilterOptions
            noun="author"
            emptyNote="Authors appear here as pull requests load. Type a login to filter by hand."
            selected={filters.authors}
            onChange={(authors) => onChange({ ...filters, authors })}
          />
        </FilterSection>
        <div className="mt-3 flex flex-col gap-2 border-t border-border-subtle pt-3">
          <FilterSwitch
            label="Needs attention"
            checked={filters.attentionOnly}
            onCheckedChange={(attentionOnly) =>
              onChange({ ...filters, attentionOnly })
            }
          />
          <FilterSwitch
            label="Ready to merge"
            checked={filters.readyOnly}
            onCheckedChange={(readyOnly) => onChange({ ...filters, readyOnly })}
          />
          <LinkedFilter
            value={filters.tidebreakLinked}
            onChange={(tidebreakLinked) =>
              onChange({ ...filters, tidebreakLinked })
            }
          />
        </div>
      </PopoverContent>
    </Popover>
  );
}

export function RunFilters({
  repositories,
  filters,
  onChange,
}: {
  repositories: CodeGitHubRepositoryRef[];
  filters: CodeDeliveryRunViewFilters;
  onChange: (filters: CodeDeliveryRunViewFilters) => void;
}) {
  const count =
    filters.repositoryKeys.length +
    filters.kinds.length +
    filters.statuses.length +
    filters.conclusions.length +
    filters.workflows.length +
    filters.environments.length +
    filters.branches.length +
    filters.events.length +
    filters.actors.length +
    Number(filters.attentionOnly) +
    Number(filters.tidebreakLinked !== undefined);
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button type="button" size="sm" variant="outline">
          <Filter />
          Filters
          {count > 0 && (
            <span className="rounded-full bg-primary px-1.5 text-2xs text-primary-foreground">
              {count}
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        className="max-h-[min(720px,calc(100vh-32px))] w-[24rem] max-w-[calc(100vw-24px)] overflow-auto p-3"
      >
        <FilterSection title="Repositories">
          <RepositoryCheckboxes
            repositories={repositories}
            selected={filters.repositoryKeys}
            onChange={(repositoryKeys) =>
              onChange({ ...filters, repositoryKeys })
            }
          />
        </FilterSection>
        <FilterSection title="Kind">
          <CheckboxOptions
            options={["workflow_run", "deployment"]}
            selected={filters.kinds}
            onChange={(kinds) =>
              onChange({ ...filters, kinds: kinds as CodeDeliveryRunKind[] })
            }
          />
        </FilterSection>
        <FilterSection title="Status">
          <CheckboxOptions
            options={["queued", "in_progress", "completed", "pending"]}
            selected={filters.statuses}
            onChange={(statuses) => onChange({ ...filters, statuses })}
          />
        </FilterSection>
        <FilterSection title="Conclusion">
          <CheckboxOptions
            options={[
              "success",
              "failure",
              "cancelled",
              "timed_out",
              "action_required",
              "startup_failure",
            ]}
            selected={filters.conclusions}
            onChange={(conclusions) => onChange({ ...filters, conclusions })}
          />
        </FilterSection>
        <AdvancedTextFilter
          label="Workflows"
          value={filters.workflows}
          placeholder="CI, Release"
          onChange={(workflows) => onChange({ ...filters, workflows })}
        />
        <AdvancedTextFilter
          label="Environments"
          value={filters.environments}
          placeholder="production, staging"
          onChange={(environments) => onChange({ ...filters, environments })}
        />
        <AdvancedTextFilter
          label="Branches"
          value={filters.branches}
          placeholder="main, release/*"
          onChange={(branches) => onChange({ ...filters, branches })}
        />
        <AdvancedTextFilter
          label="Events"
          value={filters.events}
          placeholder="push, pull_request"
          onChange={(events) => onChange({ ...filters, events })}
        />
        <FilterSection title="Actors">
          <AuthorFilterOptions
            noun="actor"
            emptyNote="Actors appear here as runs load. Type a login to filter by hand."
            selected={filters.actors}
            onChange={(actors) => onChange({ ...filters, actors })}
          />
        </FilterSection>
        <div className="mt-3 flex flex-col gap-2 border-t border-border-subtle pt-3">
          <FilterSwitch
            label="Needs attention"
            checked={filters.attentionOnly}
            onCheckedChange={(attentionOnly) =>
              onChange({ ...filters, attentionOnly })
            }
          />
          <LinkedFilter
            value={filters.tidebreakLinked}
            onChange={(tidebreakLinked) =>
              onChange({ ...filters, tidebreakLinked })
            }
          />
        </div>
      </PopoverContent>
    </Popover>
  );
}

function FilterSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <fieldset className="mb-3 border-b border-border-subtle pb-3 last:mb-0 last:border-b-0 last:pb-0">
      <legend className="mb-2 text-xs font-medium text-muted-foreground">
        {title}
      </legend>
      {children}
    </fieldset>
  );
}

function RepositoryCheckboxes({
  repositories,
  selected,
  onChange,
}: {
  repositories: CodeGitHubRepositoryRef[];
  selected: string[];
  onChange: (selected: string[]) => void;
}) {
  if (repositories.length === 0) {
    return (
      <p className="text-xs text-muted-foreground">No tracked repositories.</p>
    );
  }
  return (
    <div className="flex max-h-36 flex-col gap-1 overflow-auto pr-1">
      {repositories.map((repository) => {
        const key = codeDeliveryRepositoryKey(repository);
        return (
          <label
            key={key}
            className="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 text-xs hover:bg-muted/40"
          >
            <Checkbox
              checked={selected.includes(key)}
              onCheckedChange={(checked) =>
                onChange(toggleValue(selected, key, checked === true))
              }
            />
            <span className="min-w-0 truncate">
              {repository.name_with_owner}
            </span>
          </label>
        );
      })}
    </div>
  );
}

/**
 * Login selection without the memory test: the checkable pool is every login
 * Delivery has seen on a pull request or run, drawn with avatars, and the
 * search box narrows it. A login the pool has never seen — a teammate who has
 * not pushed lately, a bot — can still be typed and added by hand, which is
 * all the old free-text field could do.
 */
function AuthorFilterOptions({
  noun,
  emptyNote,
  selected,
  onChange,
}: {
  noun: "author" | "actor";
  emptyNote: string;
  selected: string[];
  onChange: (selected: string[]) => void;
}) {
  const knownAuthors = useCodeDeliveryStore((state) => state.knownAuthors);
  const [query, setQuery] = useState("");
  const trimmed = query.trim();
  const isSelected = (login: string) =>
    selected.some((entry) => entry.toLowerCase() === login.toLowerCase());

  const options = useMemo(() => {
    // Selected logins stay listed even when the pool has never seen them —
    // a saved view's author must be visible to be uncheckable.
    const byKey = new Map<string, CodeDeliveryAuthor>();
    for (const login of selected) byKey.set(login.toLowerCase(), { login });
    for (const author of knownAuthors) {
      const key = author.login.toLowerCase();
      const existing = byKey.get(key);
      if (!existing) byKey.set(key, author);
      else if (author.avatarUrl && !existing.avatarUrl) {
        byKey.set(key, { ...existing, avatarUrl: author.avatarUrl });
      }
    }
    const needle = trimmed.toLowerCase();
    const chosen = new Set(selected.map((entry) => entry.toLowerCase()));
    return [...byKey.values()]
      .filter(
        (author) => !needle || author.login.toLowerCase().includes(needle),
      )
      .sort((left, right) => {
        const bySelection =
          Number(chosen.has(right.login.toLowerCase())) -
          Number(chosen.has(left.login.toLowerCase()));
        if (bySelection !== 0) return bySelection;
        return left.login.localeCompare(right.login);
      });
  }, [knownAuthors, selected, trimmed]);

  const toggle = (login: string, enabled: boolean) => {
    const rest = selected.filter(
      (entry) => entry.toLowerCase() !== login.toLowerCase(),
    );
    onChange(enabled ? [...rest, login] : rest);
  };

  const exactMatchListed = options.some(
    (author) => author.login.toLowerCase() === trimmed.toLowerCase(),
  );
  const addTyped = () => {
    if (!trimmed) return;
    toggle(trimmed, true);
    setQuery("");
  };

  return (
    <div className="flex flex-col gap-1.5">
      <Input
        value={query}
        placeholder={`Search ${noun}s or type a login`}
        aria-label={`Search ${noun}s`}
        onChange={(event) => setQuery(event.target.value)}
        onKeyDown={(event) => {
          if (event.key !== "Enter") return;
          event.preventDefault();
          if (!exactMatchListed) addTyped();
          else if (trimmed) {
            toggle(trimmed, !isSelected(trimmed));
            setQuery("");
          }
        }}
      />
      {options.length > 0 && (
        <div className="flex max-h-36 flex-col gap-1 overflow-auto pr-1">
          {options.map((author) => (
            <label
              key={author.login.toLowerCase()}
              className="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 text-xs hover:bg-muted/40"
            >
              <Checkbox
                checked={isSelected(author.login)}
                onCheckedChange={(checked) =>
                  toggle(author.login, checked === true)
                }
              />
              <GithubAvatar login={author.login} url={author.avatarUrl} />
              <span className="min-w-0 truncate">{author.login}</span>
            </label>
          ))}
        </div>
      )}
      {options.length === 0 && !trimmed && (
        <p className="text-xs text-muted-foreground">{emptyNote}</p>
      )}
      {trimmed && !exactMatchListed && (
        <button
          type="button"
          className="cursor-pointer rounded-md px-1.5 py-1 text-left text-xs text-muted-foreground hover:bg-muted/40 hover:text-foreground"
          onClick={addTyped}
        >
          Filter by “{trimmed}”
        </button>
      )}
    </div>
  );
}

function CheckboxOptions<T extends string>({
  options,
  selected,
  onChange,
}: {
  options: readonly T[];
  selected: readonly T[];
  onChange: (selected: T[]) => void;
}) {
  return (
    <div className="grid grid-cols-2 gap-1">
      {options.map((option) => (
        <label
          key={option}
          className="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 text-xs hover:bg-muted/40"
        >
          <Checkbox
            checked={selected.includes(option)}
            onCheckedChange={(checked) =>
              onChange(toggleValue([...selected], option, checked === true))
            }
          />
          <span>{humanize(option)}</span>
        </label>
      ))}
    </div>
  );
}

function FilterSwitch({
  label,
  checked,
  onCheckedChange,
}: {
  label: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center justify-between gap-3 text-xs">
      <span>{label}</span>
      <Switch
        checked={checked}
        onCheckedChange={onCheckedChange}
        aria-label={label}
      />
    </label>
  );
}

function LinkedFilter({
  value,
  onChange,
}: {
  value: boolean | undefined;
  onChange: (value: boolean | undefined) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="text-xs">Tidebreak link</span>
      <Select
        value={value === undefined ? "any" : value ? "linked" : "unlinked"}
        onValueChange={(next) =>
          onChange(next === "any" ? undefined : next === "linked")
        }
      >
        <SelectTrigger size="sm" className="w-28">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="any">Any</SelectItem>
          <SelectItem value="linked">Linked</SelectItem>
          <SelectItem value="unlinked">Unlinked</SelectItem>
        </SelectContent>
      </Select>
    </div>
  );
}

function AdvancedTextFilter({
  label,
  value,
  placeholder,
  onChange,
}: {
  label: string;
  value: string[];
  placeholder: string;
  onChange: (value: string[]) => void;
}) {
  return (
    <div className="mb-3 flex flex-col gap-1.5">
      <Label className="text-xs text-muted-foreground">{label}</Label>
      <Input
        value={value.join(", ")}
        placeholder={placeholder}
        onChange={(event) => onChange(commaList(event.target.value))}
      />
    </div>
  );
}
