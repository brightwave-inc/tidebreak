import { useEffect, useState, type FormEvent } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";

import type {
  CodePermissionMode,
  CodeRepoSnapshot,
  HarnessKind,
} from "../api/types";
import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { friendlyErrorMessage } from "@/lib/utils";
import { PermissionModePicker } from "./CodeComposer";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { HarnessPicker } from "./HarnessPicker";
import {
  autoIsUnsupervised,
  createPermissionModes,
  defaultCreatePermissionMode,
  harnessUnusableReason,
  UNSUPERVISED_AUTO_NOTE,
} from "./labels";

/**
 * Create a workspace and its first session, then open it.
 *
 * The title is optional: left blank, the server generates a two-word name and
 * later replaces it with one derived from the first turn, the same way chats
 * are named. Typing a title here is the way to opt out of that.
 *
 * Permission mode defaults to Ask when the doctor reports structured
 * approvals, otherwise Plan — create always has a mode the harness can honor.
 * The harness picker lists every doctor entry. Ready rows are selectable;
 * unusable ones stay visible and dimmed.
 */

export function NewWorkspaceDialog({
  open,
  onOpenChange,
  repos,
  defaultRepoId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  repos: CodeRepoSnapshot[];
  defaultRepoId?: string;
}) {
  const navigate = useNavigate();
  const { client } = useApp();
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const upsertWorkspace = useCodeCatalogStore((state) => state.upsertWorkspace);
  const rememberSession = useCodeCatalogStore((state) => state.rememberSession);
  const [repoId, setRepoId] = useState(defaultRepoId ?? repos[0]?.id ?? "");
  const [title, setTitle] = useState("");
  const [baseRef, setBaseRef] = useState("");
  const [harness, setHarness] = useState<HarnessKind>("claude_code");
  const [permissionMode, setPermissionMode] = useState<CodePermissionMode | null>(
    null,
  );
  const [creating, setCreating] = useState(false);

  const allHarnesses = doctor?.harnesses ?? [];
  const readyHarnesses = allHarnesses.filter(
    (entry) => !harnessUnusableReason(entry),
  );

  useEffect(() => {
    if (!open) return;
    setRepoId(defaultRepoId ?? repos[0]?.id ?? "");
    setTitle("");
    const selected = repos.find((repo) => repo.id === (defaultRepoId ?? repos[0]?.id));
    setBaseRef(selected?.default_base_ref ?? "");
    setHarness(readyHarnesses[0]?.kind ?? "claude_code");
    setPermissionMode(null);
    // Reset against the dialog opening, not against doctor refreshes mid-open.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, defaultRepoId, repos]);

  const selectedRepo = repos.find((repo) => repo.id === repoId);
  const selectedHarness = readyHarnesses.find((entry) => entry.kind === harness);
  const availableModes = selectedHarness
    ? createPermissionModes(selectedHarness.caps)
    : [];
  const postedMode =
    permissionMode && availableModes.includes(permissionMode)
      ? permissionMode
      : selectedHarness
        ? defaultCreatePermissionMode(selectedHarness.caps)
        : "plan";
  const canCreate =
    Boolean(repoId && selectedRepo && selectedHarness) && !creating;

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!canCreate) return;
    setCreating(true);
    try {
      const workspace = await client.createCodeWorkspace({
        repo_id: repoId,
        title: title.trim() || undefined,
        base_ref: baseRef.trim() || undefined,
      });
      upsertWorkspace(workspace);
      const session = await client.createCodeSession(workspace.id, {
        harness,
        permission_mode: postedMode,
      });
      rememberSession(session);
      onOpenChange(false);
      await navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId: workspace.id },
      });
    } catch (error) {
      toast.error(friendlyErrorMessage(error, "Could not create the workspace"));
    } finally {
      setCreating(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={creating ? undefined : onOpenChange}>
      <DialogContent className="max-w-md gap-5 p-5" aria-busy={creating}>
        <DialogHeader>
          <DialogTitle>New workspace</DialogTitle>
          <DialogDescription>
            One worktree and one session on the selected repo.
          </DialogDescription>
        </DialogHeader>
        <form className="flex flex-col gap-3" onSubmit={submit}>
          <div className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Repo</span>
            <Select
              value={repoId || undefined}
              onValueChange={(value) => {
                setRepoId(value);
                const next = repos.find((repo) => repo.id === value);
                if (next) setBaseRef(next.default_base_ref);
              }}
              disabled={creating || Boolean(defaultRepoId) || repos.length === 0}
            >
              <SelectTrigger aria-label="Repo">
                <SelectValue placeholder="No repos" />
              </SelectTrigger>
              <SelectContent scrollButtons={false}>
                {repos.map((repo) => (
                  <SelectItem key={repo.id} value={repo.id}>
                    {repo.display_name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Title</span>
            <Input
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              disabled={creating}
              placeholder="Named automatically"
              autoFocus
            />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Base ref</span>
            <Input
              value={baseRef}
              onChange={(event) => setBaseRef(event.target.value)}
              disabled={creating}
              placeholder={selectedRepo?.default_base_ref}
            />
          </label>
          <div className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Harness</span>
            <HarnessPicker
              harnesses={allHarnesses}
              value={harness}
              onChange={setHarness}
              disabled={creating}
            />
          </div>
          <div className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Permission mode</span>
            <PermissionModePicker
              value={postedMode}
              availableModes={availableModes}
              onChange={setPermissionMode}
            />
            {postedMode === "auto" &&
              selectedHarness &&
              autoIsUnsupervised(selectedHarness.caps) && (
                <p className="text-warning-foreground text-xs">
                  {UNSUPERVISED_AUTO_NOTE}
                </p>
              )}
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="ghost"
              onClick={() => onOpenChange(false)}
              disabled={creating}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!canCreate}>
              {creating ? "Creating…" : "Create"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
