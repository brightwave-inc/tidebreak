import { useEffect, useRef, useState } from "react";
import { CircleAlert, LoaderCircle, Plus, X } from "lucide-react";

import type { ApiClient } from "@/api/client";
import type { CodeRepoSnapshot, QuickAction } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { friendlyErrorMessage } from "@/lib/utils";

type RepoSettingsClient = Pick<ApiClient, "getCodeRepo" | "patchCodeRepo">;

/**
 * How many quick actions a repository takes. Mirrors the server's cap in
 * `routes/code/repos.rs` so the editor stops at the limit rather than letting
 * the user type a 33rd row and learn about it from a 400.
 */
const MAX_QUICK_ACTIONS = 32;

/** The editable half of a repo record, held while the user types. */
type RepoSettingsDraft = {
  default_base_ref: string;
  branch_prefix: string;
  setup_script: string;
  archive_script: string;
  quick_actions: QuickAction[];
};

function draftOf(repo: CodeRepoSnapshot): RepoSettingsDraft {
  return {
    default_base_ref: repo.default_base_ref,
    branch_prefix: repo.branch_prefix,
    setup_script: repo.setup_script ?? "",
    archive_script: repo.archive_script ?? "",
    quick_actions: repo.quick_actions.map((action) => ({ ...action })),
  };
}

/** Exactly what a draft puts on the wire. */
type RepoSettingsPayload = {
  default_base_ref?: string;
  branch_prefix?: string;
  setup_script: string | null;
  archive_script: string | null;
  quick_actions: QuickAction[];
};

/**
 * Trim a draft into a body the server will accept.
 *
 * A quick action the user is still filling in has no name or no command yet,
 * and the server rejects either. Those rows stay on screen and off the wire
 * until they are complete, so adding one never blocks saving the rest.
 */
function payloadOf(draft: RepoSettingsDraft): RepoSettingsPayload {
  return {
    default_base_ref: draft.default_base_ref.trim() || undefined,
    branch_prefix: draft.branch_prefix.trim() || undefined,
    setup_script: draft.setup_script.trim() || null,
    archive_script: draft.archive_script.trim() || null,
    quick_actions: draft.quick_actions
      .map((action) => ({
        name: action.name.trim(),
        command: action.command.trim(),
        auto_run_on_create: action.auto_run_on_create,
      }))
      .filter((action) => action.name && action.command),
  };
}

/**
 * True when the draft is worth sending. Every field is optional on the wire,
 * so a no-op blur would otherwise PATCH the repo on every focus change.
 */
function changed(draft: RepoSettingsDraft, repo: CodeRepoSnapshot): boolean {
  return (
    JSON.stringify(payloadOf(draft)) !==
    JSON.stringify(payloadOf(draftOf(repo)))
  );
}

/**
 * Repo lifecycle hooks: the base a workspace branches from, the prefix its
 * branch carries, the scripts that run around the worktree, and the named
 * commands a workspace can run.
 *
 * The server has run all four since code mode shipped; this is the only place
 * that writes them. Text commits on blur and switches commit on change, so
 * there is no Save button to forget.
 */
export function RepositorySettings({
  client,
  repoId,
  repoLabel,
  onSaved,
}: {
  client: RepoSettingsClient;
  repoId: string | null;
  repoLabel: string;
  onSaved?: (repo: CodeRepoSnapshot) => void;
}) {
  const [draft, setDraft] = useState<RepoSettingsDraft | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const busyRef = useRef(false);
  /**
   * The stored record. A ref, not state: nothing renders it, and the commit
   * that queues behind an in-flight write has to compare against what the
   * server just returned rather than the value its closure captured.
   */
  const repoRef = useRef<CodeRepoSnapshot | null>(null);
  /** A draft that arrived while a write was in flight; it goes next. */
  const queued = useRef<RepoSettingsDraft | null>(null);

  const remember = (next: CodeRepoSnapshot | null) => {
    repoRef.current = next;
  };

  const load = async () => {
    const token = ++generation.current;
    if (!repoId) {
      remember(null);
      setDraft(null);
      setLoading(false);
      setError("Register this repository in Tidebreak before editing hooks.");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const next = await client.getCodeRepo(repoId);
      if (token !== generation.current) return;
      remember(next);
      setDraft(draftOf(next));
    } catch (caught) {
      if (token === generation.current) {
        setError(friendlyErrorMessage(caught, "Could not load the repo."));
      }
    } finally {
      if (token === generation.current) setLoading(false);
    }
  };

  useEffect(() => {
    busyRef.current = false;
    queued.current = null;
    setBusy(false);
    remember(null);
    setDraft(null);
    void load();
    return () => {
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, repoId]);

  /**
   * Send one draft. A rejected write leaves the draft as typed so the user can
   * fix the field the server named instead of retyping the whole form.
   *
   * Edits land faster than the round trip — a blur and the switch beside it
   * are one gesture — so a draft that arrives mid-write is held and sent when
   * the write returns rather than dropped.
   */
  const commit = async (next: RepoSettingsDraft) => {
    const stored = repoRef.current;
    if (!repoId || !stored) return;
    if (busyRef.current) {
      queued.current = next;
      return;
    }
    if (!changed(next, stored)) return;
    const token = generation.current;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const saved = await client.patchCodeRepo(repoId, payloadOf(next));
      if (token !== generation.current) return;
      remember(saved);
      // Keep the list on screen as the user has it: the rows the server just
      // echoed, plus any row still being filled in.
      setDraft({ ...draftOf(saved), quick_actions: next.quick_actions });
      onSaved?.(saved);
    } catch (caught) {
      if (token === generation.current) {
        setError(friendlyErrorMessage(caught, "Could not save the repo."));
      }
    } finally {
      if (token === generation.current) {
        busyRef.current = false;
        setBusy(false);
        const held = queued.current;
        queued.current = null;
        if (held) void commit(held);
      }
    }
  };

  const editActions = (
    apply: (actions: QuickAction[]) => QuickAction[],
    save: boolean,
  ) => {
    if (!draft) return;
    const next = { ...draft, quick_actions: apply(draft.quick_actions) };
    setDraft(next);
    if (save) void commit(next);
  };

  return (
    <div className="rounded-lg border border-border-subtle bg-background p-4">
      <div className="mb-4 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-xs font-medium text-muted-foreground">
            {repoLabel}
          </p>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Scripts run in the worktree. A failing setup script leaves the
            checkout in place and marks the workspace Setup failed.
          </p>
        </div>
        {(loading || busy) && (
          <LoaderCircle className="size-3.5 shrink-0 animate-spin" />
        )}
      </div>
      {error && (
        <div className="notice-surface notice-critical mb-4 flex flex-col items-stretch gap-2 rounded-md border px-3 py-2 text-xs min-[480px]:flex-row min-[480px]:items-center min-[480px]:justify-between">
          <span className="flex min-w-0 items-start gap-2">
            <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
            <span className="min-w-0">{error}</span>
          </span>
          <Button
            type="button"
            size="xs"
            variant="outline"
            className="shrink-0 self-end min-[480px]:self-auto"
            disabled={loading}
            onClick={() => void load()}
          >
            Try again
          </Button>
        </div>
      )}
      {!loading && draft && (
        <div className="flex flex-col gap-4">
          <div className="grid grid-cols-1 gap-3 min-[520px]:grid-cols-2">
            <label className="flex min-w-0 flex-col gap-1.5">
              <span className="text-xs text-muted-foreground">Base ref</span>
              <Input
                className="font-mono"
                value={draft.default_base_ref}
                placeholder="main"
                onChange={(event) =>
                  setDraft({ ...draft, default_base_ref: event.target.value })
                }
                onBlur={() => void commit(draft)}
              />
            </label>
            <label className="flex min-w-0 flex-col gap-1.5">
              <span className="text-xs text-muted-foreground">
                Branch prefix
              </span>
              <Input
                className="font-mono"
                value={draft.branch_prefix}
                placeholder="tidebreak/"
                onChange={(event) =>
                  setDraft({ ...draft, branch_prefix: event.target.value })
                }
                onBlur={() => void commit(draft)}
              />
            </label>
          </div>
          <div className="flex min-w-0 flex-col gap-1.5">
            <label className="flex min-w-0 flex-col gap-1.5">
              <span className="text-xs text-muted-foreground">
                Setup script
              </span>
              <Textarea
                className="min-h-16 font-mono"
                rows={3}
                value={draft.setup_script}
                placeholder="pnpm install"
                onChange={(event) =>
                  setDraft({ ...draft, setup_script: event.target.value })
                }
                onBlur={() => void commit(draft)}
              />
            </label>
            <p className="text-xs text-muted-foreground">
              Runs after a worktree is created or restored. Tidebreak sets
              TIDEBREAK_REPO_ROOT and TIDEBREAK_WORKSPACE_NAME.
            </p>
          </div>
          <div className="flex min-w-0 flex-col gap-1.5">
            <label className="flex min-w-0 flex-col gap-1.5">
              <span className="text-xs text-muted-foreground">
                Archive script
              </span>
              <Textarea
                className="min-h-16 font-mono"
                rows={3}
                value={draft.archive_script}
                placeholder="./scripts/back-up.sh"
                onChange={(event) =>
                  setDraft({ ...draft, archive_script: event.target.value })
                }
                onBlur={() => void commit(draft)}
              />
            </label>
            <p className="text-xs text-muted-foreground">
              Runs before the worktree is removed. A failure stops the archive.
              Tidebreak sets TIDEBREAK_REPO_ROOT and TIDEBREAK_WORKSPACE_NAME.
            </p>
          </div>
          <div className="flex flex-col gap-2">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-muted-foreground">
                Quick actions
              </span>
              <Button
                type="button"
                size="xs"
                variant="outline"
                disabled={draft.quick_actions.length >= MAX_QUICK_ACTIONS}
                onClick={() =>
                  editActions(
                    (actions) => [
                      ...actions,
                      { name: "", command: "", auto_run_on_create: false },
                    ],
                    false,
                  )
                }
              >
                <Plus />
                Add
              </Button>
            </div>
            {draft.quick_actions.length >= MAX_QUICK_ACTIONS && (
              <p className="text-xs text-muted-foreground">
                A repository takes at most {MAX_QUICK_ACTIONS} quick actions.
              </p>
            )}
            {draft.quick_actions.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                No quick actions yet. Add one to run a named command in any
                workspace of this repo.
              </p>
            ) : (
              <div className="flex flex-col gap-2">
                {draft.quick_actions.map((action, index) => (
                  <div
                    // Names are still being typed here, so the row's position
                    // is the only stable identity it has.
                    // eslint-disable-next-line react/no-array-index-key
                    key={index}
                    className="flex min-w-0 flex-wrap items-center gap-2"
                  >
                    <Input
                      className="min-w-24 flex-1 basis-28"
                      value={action.name}
                      placeholder="Test"
                      aria-label={`Quick action ${index + 1} name`}
                      onChange={(event) =>
                        editActions(
                          (actions) =>
                            actions.map((item, at) =>
                              at === index
                                ? { ...item, name: event.target.value }
                                : item,
                            ),
                          false,
                        )
                      }
                      onBlur={() => draft && void commit(draft)}
                    />
                    <Input
                      className="min-w-0 flex-[2] basis-40 font-mono"
                      value={action.command}
                      placeholder="cargo test"
                      aria-label={`Quick action ${index + 1} command`}
                      onChange={(event) =>
                        editActions(
                          (actions) =>
                            actions.map((item, at) =>
                              at === index
                                ? { ...item, command: event.target.value }
                                : item,
                            ),
                          false,
                        )
                      }
                      onBlur={() => draft && void commit(draft)}
                    />
                    <label className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
                      <Switch
                        checked={action.auto_run_on_create}
                        aria-label={`Run quick action ${index + 1} on create`}
                        onCheckedChange={(checked) =>
                          editActions(
                            (actions) =>
                              actions.map((item, at) =>
                                at === index
                                  ? { ...item, auto_run_on_create: checked }
                                  : item,
                              ),
                            true,
                          )
                        }
                      />
                      On create
                    </label>
                    <Button
                      type="button"
                      size="icon-xs"
                      variant="ghost-destructive"
                      aria-label={`Remove quick action ${index + 1}`}
                      onClick={() =>
                        editActions(
                          (actions) =>
                            actions.filter((_item, at) => at !== index),
                          true,
                        )
                      }
                    >
                      <X />
                    </Button>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
