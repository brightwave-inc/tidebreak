import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import type {
  CodeDeliverySourceError,
  CodeGitHubRepositoryRef,
} from "../../api/types";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { LoaderCircle, Pin, PinOff, RefreshCw, X } from "lucide-react";
import { RepositorySettings } from "../RepositorySettings";
import { RepositoryTriggerRules } from "../RepositoryTriggerRules";
import {
  codeDeliveryRepositoryKey,
  useCodeDeliveryStore,
} from "../CodeDeliveryStore";
import { codeTriggerTargetForRepository } from "../CodeTriggerTarget";
import { friendlyErrorMessage } from "@/lib/utils";
import { useApp } from "@/AppContext";
import { useCodeCatalogStore } from "../CodeCatalogStore";
import { useCodeUpdatesStore } from "../CodeUpdatesStore";
import { useMemo, useState } from "react";

export function DeliveryRepositoriesDialog({
  open,
  onOpenChange,
  discovered,
  onResolved,
  onRefresh,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  discovered: CodeGitHubRepositoryRef[];
  onResolved: (
    repositories: CodeGitHubRepositoryRef[],
    errors: CodeDeliverySourceError[],
  ) => void;
  onRefresh: () => void;
}) {
  const { client } = useApp();
  const workspaces = useCodeCatalogStore((state) => state.workspaces);
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const conversationsByWorkspace = useCodeUpdatesStore(
    (state) => state.conversationsByWorkspace,
  );
  const manualRepositories = useCodeDeliveryStore(
    (state) => state.manualRepositories,
  );
  const excluded = useCodeDeliveryStore(
    (state) => state.excludedRegisteredRepoIds,
  );
  const pinned = useCodeDeliveryStore((state) => state.pinnedRepositoryKeys);
  const [input, setInput] = useState("");
  const [resolving, setResolving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [triggerRepository, setTriggerRepository] =
    useState<CodeGitHubRepositoryRef | null>(null);
  const [settingsRepository, setSettingsRepository] =
    useState<CodeGitHubRepositoryRef | null>(null);
  const triggerTarget = useMemo(
    () =>
      codeTriggerTargetForRepository({
        repoId: triggerRepository?.tidebreak_repo_id,
        workspaces,
        conversationsByWorkspace,
        doctor,
      }),
    [conversationsByWorkspace, doctor, triggerRepository, workspaces],
  );

  const add = async () => {
    const repositories = input
      .split(/[\n,]+/)
      .map((item) => item.trim())
      .filter(Boolean);
    if (repositories.length === 0 || resolving) return;
    setResolving(true);
    setError(null);
    try {
      const snapshot =
        await client.resolveCodeDeliveryRepositories(repositories);
      onResolved(snapshot.repositories, snapshot.errors);
      if (snapshot.repositories.length > 0) setInput("");
      if (snapshot.errors.length > 0) {
        setError(snapshot.errors.map((item) => item.message).join(" "));
      }
    } catch (caught) {
      setError(
        friendlyErrorMessage(caught, "Could not resolve those repositories."),
      );
    } finally {
      setResolving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Tracked repositories</DialogTitle>
        </DialogHeader>
        <div className="flex max-h-[65vh] flex-col gap-5 overflow-auto pr-1">
          <section>
            <div className="flex items-center justify-between gap-2">
              <div>
                <h3 className="text-sm font-medium">Registered in Tidebreak</h3>
                <p className="text-xs text-muted-foreground">
                  GitHub repos are tracked automatically. Disable any you do not
                  want listed here.
                </p>
              </div>
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                onClick={onRefresh}
              >
                <RefreshCw />
                <span className="sr-only">Refresh repositories</span>
              </Button>
            </div>
            <div className="mt-3 flex flex-col rounded-lg border border-border-subtle">
              {discovered.length === 0 ? (
                <p className="px-3 py-4 text-xs text-muted-foreground">
                  No registered repositories resolve to GitHub yet.
                </p>
              ) : (
                discovered.map((repository) => {
                  const key = codeDeliveryRepositoryKey(repository);
                  const enabled =
                    !repository.tidebreak_repo_id ||
                    !excluded.includes(repository.tidebreak_repo_id);
                  return (
                    <RepositorySettingRow
                      key={key}
                      repository={repository}
                      enabled={enabled}
                      pinned={pinned.includes(key)}
                      onEnabledChange={(next) => {
                        if (!repository.tidebreak_repo_id) return;
                        useCodeDeliveryStore
                          .getState()
                          .setRegisteredRepositoryExcluded(
                            repository.tidebreak_repo_id,
                            !next,
                          );
                      }}
                      onPinnedChange={(next) =>
                        useCodeDeliveryStore
                          .getState()
                          .setRepositoryPinned(key, next)
                      }
                      onManageTriggers={
                        repository.tidebreak_repo_id
                          ? () => setTriggerRepository(repository)
                          : undefined
                      }
                      onManageSettings={
                        repository.tidebreak_repo_id
                          ? () => setSettingsRepository(repository)
                          : undefined
                      }
                    />
                  );
                })
              )}
            </div>
            {settingsRepository && (
              <div className="mt-4">
                <RepositorySettings
                  client={client}
                  repoId={settingsRepository.tidebreak_repo_id ?? null}
                  repoLabel={settingsRepository.name_with_owner}
                  onSaved={(repo) =>
                    useCodeCatalogStore.getState().upsertRepo(repo)
                  }
                />
              </div>
            )}
            {triggerRepository && (
              <div className="mt-4">
                <RepositoryTriggerRules
                  client={client}
                  repository={triggerRepository}
                  target={triggerTarget}
                />
              </div>
            )}
          </section>

          <section>
            <h3 className="text-sm font-medium">Other GitHub repositories</h3>
            <p className="text-xs text-muted-foreground">
              Add owner/repo, a GitHub URL, or host/owner/repo. One per line.
            </p>
            <div className="mt-3 flex gap-2">
              <Input
                value={input}
                placeholder="brightwave-inc/tidebreak"
                onChange={(event) => setInput(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void add();
                  }
                }}
              />
              <Button
                type="button"
                disabled={resolving || !input.trim()}
                onClick={() => void add()}
              >
                {resolving && <LoaderCircle className="animate-spin" />}
                Add
              </Button>
            </div>
            {error && <p className="mt-2 text-xs text-critical">{error}</p>}
            {manualRepositories.length > 0 && (
              <div className="mt-3 flex flex-col rounded-lg border border-border-subtle">
                {manualRepositories.map((repository) => {
                  const key = codeDeliveryRepositoryKey(repository);
                  return (
                    <RepositorySettingRow
                      key={key}
                      repository={repository}
                      enabled
                      pinned={pinned.includes(key)}
                      manual
                      onEnabledChange={() => {}}
                      onPinnedChange={(next) =>
                        useCodeDeliveryStore
                          .getState()
                          .setRepositoryPinned(key, next)
                      }
                      onRemove={() =>
                        useCodeDeliveryStore
                          .getState()
                          .removeManualRepository(key)
                      }
                    />
                  );
                })}
              </div>
            )}
          </section>
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            Done
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function RepositorySettingRow({
  repository,
  enabled,
  pinned,
  manual = false,
  onEnabledChange,
  onPinnedChange,
  onRemove,
  onManageTriggers,
  onManageSettings,
}: {
  repository: CodeGitHubRepositoryRef;
  enabled: boolean;
  pinned: boolean;
  manual?: boolean;
  onEnabledChange: (enabled: boolean) => void;
  onPinnedChange: (pinned: boolean) => void;
  onRemove?: () => void;
  onManageTriggers?: () => void;
  onManageSettings?: () => void;
}) {
  return (
    <div className="flex items-center gap-3 border-b border-border-subtle px-3 py-2.5 last:border-b-0">
      {!manual && (
        <Checkbox
          checked={enabled}
          onCheckedChange={(checked) => onEnabledChange(checked === true)}
          aria-label={`Track ${repository.name_with_owner}`}
        />
      )}
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">
          {repository.name_with_owner}
        </p>
        <p className="truncate text-xs text-muted-foreground">
          {repository.host}
        </p>
      </div>
      <Button
        type="button"
        size="icon-xs"
        variant="ghost"
        aria-label={pinned ? "Unpin repository" : "Pin repository"}
        onClick={() => onPinnedChange(!pinned)}
      >
        {pinned ? <PinOff /> : <Pin />}
      </Button>
      {onManageSettings && (
        <Button
          type="button"
          size="xs"
          variant="outline"
          onClick={onManageSettings}
        >
          Settings
        </Button>
      )}
      {onManageTriggers && (
        <Button
          type="button"
          size="xs"
          variant="outline"
          onClick={onManageTriggers}
        >
          Triggers
        </Button>
      )}
      {manual && onRemove && (
        <Button
          type="button"
          size="icon-xs"
          variant="ghost-destructive"
          aria-label="Remove repository"
          onClick={onRemove}
        >
          <X />
        </Button>
      )}
    </div>
  );
}
