import { useState } from "react";

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
import { Label } from "@/components/ui/label";
import {
  parseInstallOutcome,
  plainGitInstallError,
  validateGitInstallInput,
  type GitInstallPhase,
  type PluginInstallOutcome,
} from "./gitInstall";

export function AddFromGitDialog({
  open,
  onOpenChange,
  install,
  onInstalled,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  install: (url: string, revision: string) => Promise<unknown>;
  onInstalled: () => void;
}) {
  const [url, setUrl] = useState("");
  const [revision, setRevision] = useState("");
  const [phase, setPhase] = useState<GitInstallPhase>({ status: "empty" });

  const reset = () => {
    setUrl("");
    setRevision("");
    setPhase({ status: "empty" });
  };

  const submit = async () => {
    setPhase({ status: "validating" });
    const problem = validateGitInstallInput(url, revision);
    if (problem) {
      setPhase({ status: "failed", message: problem });
      return;
    }
    setPhase({ status: "installing" });
    try {
      const outcome = parseInstallOutcome(
        await install(url.trim(), revision.trim()),
      );
      setPhase({ status: "installed", outcome });
      onInstalled();
    } catch (error) {
      setPhase({ status: "failed", message: plainGitInstallError(error) });
    }
  };

  return (
    <AddFromGitDialogView
      open={open}
      url={url}
      revision={revision}
      phase={phase}
      onUrlChange={(value) => {
        setUrl(value);
        if (phase.status === "failed" || phase.status === "installed") {
          setPhase({ status: "empty" });
        }
      }}
      onRevisionChange={(value) => {
        setRevision(value);
        if (phase.status === "failed" || phase.status === "installed") {
          setPhase({ status: "empty" });
        }
      }}
      onSubmit={() => void submit()}
      onOpenChange={(next) => {
        if (!next) reset();
        onOpenChange(next);
      }}
    />
  );
}

export function AddFromGitDialogView({
  open,
  url,
  revision,
  phase,
  onUrlChange,
  onRevisionChange,
  onSubmit,
  onOpenChange,
}: {
  open: boolean;
  url: string;
  revision: string;
  phase: GitInstallPhase;
  onUrlChange: (value: string) => void;
  onRevisionChange: (value: string) => void;
  onSubmit: () => void;
  onOpenChange: (open: boolean) => void;
}) {
  const busy = phase.status === "validating" || phase.status === "installing";
  const installed = phase.status === "installed" ? phase.outcome : null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Add from Git</DialogTitle>
          <DialogDescription>
            Pin a public HTTPS repository to a tag or a full commit SHA. The
            plugin's files run with the agent's permissions.
          </DialogDescription>
        </DialogHeader>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (!busy && !installed) onSubmit();
          }}
        >
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="plugin-git-url">Repository URL</Label>
            <Input
              id="plugin-git-url"
              type="url"
              autoComplete="off"
              spellCheck={false}
              placeholder="https://github.com/acme/notes"
              value={url}
              disabled={busy || installed !== null}
              onChange={(event) => onUrlChange(event.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="plugin-git-ref">Tag or commit SHA</Label>
            <Input
              id="plugin-git-ref"
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder="v1.0.0"
              value={revision}
              disabled={busy || installed !== null}
              onChange={(event) => onRevisionChange(event.target.value)}
            />
            <p className="text-muted-foreground text-xs">
              Use a tag or a full commit SHA. A moving branch is not accepted.
            </p>
          </div>
          {phase.status === "validating" && (
            <p role="status" className="text-muted-foreground text-sm">
              Checking the URL…
            </p>
          )}
          {phase.status === "installing" && (
            <p role="status" className="text-muted-foreground text-sm">
              Installing from Git…
            </p>
          )}
          {phase.status === "failed" && (
            <p role="alert" className="text-critical text-sm">
              {phase.message}
            </p>
          )}
          {installed && <InstalledReport outcome={installed} />}
          <DialogFooter>
            <Button
              type="button"
              variant="ghost"
              onClick={() => onOpenChange(false)}
            >
              {installed ? "Close" : "Cancel"}
            </Button>
            {!installed && (
              <Button type="submit" disabled={busy}>
                {busy ? "Installing…" : "Add plugin"}
              </Button>
            )}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function InstalledReport({ outcome }: { outcome: PluginInstallOutcome }) {
  return (
    <div className="flex flex-col gap-2" role="status">
      <p className="text-sm">
        Installed <span className="font-mono text-xs">{outcome.plugin}</span> at{" "}
        <span className="font-mono text-xs">{outcome.revision}</span>.
      </p>
      {outcome.skipped.length > 0 ? (
        <div className="flex flex-col gap-1.5">
          <p className="text-muted-foreground text-2xs font-medium tracking-wide uppercase">
            Skipped · {outcome.skipped.length}
          </p>
          <ul className="flex flex-col gap-2">
            {outcome.skipped.map((member) => (
              <li key={`${member.path}:${member.reason}`} className="min-w-0">
                <p className="break-all font-mono text-xs">{member.path}</p>
                <p className="text-muted-foreground break-words text-xs leading-snug">
                  {member.reason}
                </p>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="text-muted-foreground text-xs">
          No members were skipped.
        </p>
      )}
    </div>
  );
}
