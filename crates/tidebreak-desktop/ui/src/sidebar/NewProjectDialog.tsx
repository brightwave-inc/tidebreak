import { useEffect, useMemo, useState, type FormEvent, type KeyboardEvent } from "react";

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
import { usesCommandModifier } from "@/ShellShortcuts";

/** Matches `MAX_PROJECT_TITLE_CHARS` on the project create route. */
export const MAX_PROJECT_TITLE_CHARS = 120;

/**
 * Name a project before it exists.
 *
 * The rail used to drop a "New project" row straight into rename. That made
 * the first thing on the list look half-made, and it created the folder
 * before the reader had said what it was for. This dialog collects the name,
 * then the shell creates the project and the chat that belongs in it.
 */
export function NewProjectDialog({
  open,
  onOpenChange,
  onCreate,
  creating,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Resolve `true` when the project exists and the dialog should close. */
  onCreate: (title: string) => Promise<boolean>;
  creating: boolean;
}) {
  const [title, setTitle] = useState("");
  const command = useMemo(
    () => usesCommandModifier(navigator.userAgent),
    [],
  );
  const trimmed = title.trim();
  const canCreate = trimmed.length > 0 && !creating;

  useEffect(() => {
    if (!open) setTitle("");
  }, [open]);

  function requestClose(next: boolean) {
    if (creating) return;
    onOpenChange(next);
  }

  async function submit() {
    if (!canCreate) return;
    const created = await onCreate(trimmed);
    if (created) onOpenChange(false);
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    void submit();
  }

  function onKeyDown(event: KeyboardEvent) {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void submit();
    }
  }

  return (
    <Dialog open={open} onOpenChange={requestClose}>
      <DialogContent
        className="max-w-sm gap-5 p-5 sm:rounded-xl"
        aria-busy={creating}
        withCloseButton={!creating}
        onKeyDown={onKeyDown}
      >
        <DialogHeader className="gap-1 pr-6">
          <DialogTitle className="text-base">New project</DialogTitle>
          <DialogDescription className="text-xs leading-relaxed">
            Name it. New work will open inside it.
          </DialogDescription>
        </DialogHeader>

        <form className="flex flex-col gap-5" onSubmit={onSubmit}>
          <label className="flex flex-col gap-1.5">
            <span className="text-sm font-medium">Name</span>
            <Input
              autoFocus
              autoComplete="off"
              aria-label="Project name"
              placeholder="Research"
              maxLength={MAX_PROJECT_TITLE_CHARS}
              value={title}
              disabled={creating}
              onChange={(event) => setTitle(event.target.value)}
            />
          </label>

          <DialogFooter className="gap-2 sm:justify-end">
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={creating}
              onClick={() => requestClose(false)}
            >
              Cancel
            </Button>
            <Button type="submit" size="sm" disabled={!canCreate}>
              {creating ? "Creating…" : "Create"}
              {!creating && (
                <span
                  className="ml-1 inline-flex items-center gap-0.5 text-2xs font-medium opacity-60"
                  aria-hidden="true"
                >
                  <kbd className="font-sans">{command ? "⌘" : "Ctrl"}</kbd>
                  <kbd className="font-sans">↩</kbd>
                </span>
              )}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
