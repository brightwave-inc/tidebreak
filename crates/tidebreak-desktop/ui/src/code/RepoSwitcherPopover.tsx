import { useMemo, useState, type KeyboardEvent } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { ChevronDown, FolderGit2, Plus } from "lucide-react";

import {
  OptionListbox,
  optionElementId,
  type OptionRow,
} from "@/components/OptionListbox";
import { SearchInput } from "@/components/SearchInput";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import type { CodeRepoSnapshot } from "../api/types";
import { useCodeUiStore } from "./CodeUiStore";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { codeRepoIdFromPath } from "./routes";

const LIST_ID = "repo-switcher-list";

/**
 * Repo navigation, folded into one button.
 *
 * The rail used to spend a whole section on the repo list; every row of it
 * repeated what the by-repo group headers already say. This keeps repo pages
 * one click away in every sort mode: the trigger names the repo you are in
 * (or "Repos" when you are not in one), the popover is a searchable list, and
 * "Add repo" lives at the bottom — the same spot the old section's `+` served.
 *
 * The search input owns the keyboard and drives the listbox through
 * `aria-activedescendant`, the same contract `AddRepoPalette` uses.
 */
export function RepoSwitcherPopover({
  repos,
}: {
  repos: readonly CodeRepoSnapshot[];
}) {
  const navigate = useNavigate();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const setAddRepoOpen = useCodeUiStore((state) => state.setAddRepoOpen);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);

  const activeRepoId = codeRepoIdFromPath(pathname);
  const activeRepo = repos.find((repo) => repo.id === activeRepoId);

  const rows = useMemo<OptionRow[]>(() => {
    const trimmed = query.trim().toLowerCase();
    const listed = repos.filter(
      (repo) =>
        trimmed.length === 0 ||
        repo.display_name.toLowerCase().includes(trimmed),
    );
    return [
      ...listed.map((repo) => ({
        key: repo.id,
        label: repo.display_name,
        icon: FolderGit2,
        hint: repo.id === activeRepoId ? "current" : undefined,
      })),
      { key: "add-repo", label: "Add repo…", icon: Plus },
    ];
  }, [repos, query, activeRepoId]);

  function reset() {
    setQuery("");
    setActiveIndex(0);
  }

  function pick(index: number) {
    const row = rows[index];
    if (!row) return;
    setOpen(false);
    reset();
    if (row.key === "add-repo") {
      setAddRepoOpen(true);
      return;
    }
    void navigate({ to: "/code/r/$repoId", params: { repoId: row.key } });
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActiveIndex((index) => (index + 1) % rows.length);
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIndex((index) => (index - 1 + rows.length) % rows.length);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      pick(activeIndex);
    }
  }

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) reset();
      }}
    >
      <PopoverTrigger asChild>
        <button
          type="button"
          className={cn(
            "flex min-w-0 cursor-pointer items-center gap-1.5 rounded-md px-2 py-1 text-[13px] font-medium hover:bg-muted",
            FOCUS_RING,
            HOVER_TINT,
          )}
          aria-label={
            activeRepo ? `Switch repo (current: ${activeRepo.display_name})` : "Repos"
          }
        >
          <FolderGit2 className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate">
            {activeRepo ? activeRepo.display_name : "Repos"}
          </span>
          <ChevronDown className="size-3 shrink-0 text-muted-foreground" />
        </button>
      </PopoverTrigger>
      <PopoverContent
        side="bottom"
        align="start"
        sideOffset={6}
        className="flex w-64 flex-col gap-0 overflow-hidden p-0"
        onKeyDown={onKeyDown}
      >
        <div className="border-b p-2">
          <SearchInput
            size="sm"
            value={query}
            onValueChange={(next) => {
              setQuery(next);
              setActiveIndex(0);
            }}
            placeholder="Find a repo"
            aria-controls={LIST_ID}
            aria-activedescendant={
              rows[activeIndex]
                ? optionElementId(LIST_ID, activeIndex)
                : undefined
            }
          />
        </div>
        <OptionListbox
          listId={LIST_ID}
          label="Repos"
          rows={rows}
          activeIndex={activeIndex}
          note={repos.length === 0 ? "No repos registered yet." : null}
          onPick={pick}
          onHighlight={setActiveIndex}
        />
      </PopoverContent>
    </Popover>
  );
}
