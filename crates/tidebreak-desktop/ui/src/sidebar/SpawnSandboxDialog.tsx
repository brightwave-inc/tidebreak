import { type ComponentType, useState } from "react";
import { Play, Rocket } from "lucide-react";

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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { ClaudeIcon, OpenAIIcon, OpenCodeIcon, XaiIcon } from "@/ProviderIcons";

import type { SandboxHarness } from "./SandboxAgentsSection";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type SandboxProfile = {
  id: string;
  name: string;
  enabled: boolean;
};

export type SpawnSandboxRequest = {
  profile: string;
  harness: SandboxHarness;
  task: string;
  repository?: string;
  repositoryRef?: string;
  model?: string;
  reasoningEffort?: string;
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const HARNESS_OPTIONS: {
  value: SandboxHarness;
  label: string;
  icon: ComponentType<{ className?: string }>;
}[] = [
  { value: "claude_code", label: "Claude Code", icon: ClaudeIcon },
  { value: "codex", label: "Codex", icon: OpenAIIcon },
  { value: "opencode", label: "opencode", icon: OpenCodeIcon },
  { value: "grok_build", label: "Grok", icon: XaiIcon },
];

const EFFORT_OPTIONS = [
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "Extra high" },
  { value: "max", label: "Max" },
];

// ---------------------------------------------------------------------------
// Dialog
// ---------------------------------------------------------------------------

export function SpawnSandboxDialog({
  open,
  profiles,
  onSubmit,
  onClose,
}: {
  open: boolean;
  profiles: readonly SandboxProfile[];
  onSubmit: (request: SpawnSandboxRequest) => void;
  onClose: () => void;
}) {
  const [profile, setProfile] = useState(
    profiles.find((p) => p.enabled)?.id ?? "",
  );
  const [harness, setHarness] = useState<SandboxHarness>("claude_code");
  const [task, setTask] = useState("");
  const [repository, setRepository] = useState("");
  const [repositoryRef, setRepositoryRef] = useState("");
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const canSubmit = profile !== "" && task.trim() !== "" && !submitting;

  function handleSubmit() {
    if (!canSubmit) return;
    setSubmitting(true);
    onSubmit({
      profile,
      harness,
      task: task.trim(),
      repository: repository.trim() || undefined,
      repositoryRef: repositoryRef.trim() || undefined,
      model: model.trim() || undefined,
      reasoningEffort: effort || undefined,
    });
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Rocket className="size-4 text-icon-violet" aria-hidden="true" />
            Spawn sandbox agent
          </DialogTitle>
          <DialogDescription>
            Launch a detached agent in a Model Gateway sandbox. It keeps working
            after you close this window.
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-4 py-2">
          {/* Profile */}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="spawn-profile">Profile</Label>
            <Select value={profile} onValueChange={setProfile}>
              <SelectTrigger id="spawn-profile">
                <SelectValue placeholder="Select a profile" />
              </SelectTrigger>
              <SelectContent>
                {profiles.map((p) => (
                  <SelectItem key={p.id} value={p.id} disabled={!p.enabled}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {/* Harness */}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="spawn-harness">Harness</Label>
            <Select
              value={harness}
              onValueChange={(v) => setHarness(v as SandboxHarness)}
            >
              <SelectTrigger id="spawn-harness">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {HARNESS_OPTIONS.map((opt) => {
                  const Icon = opt.icon;
                  return (
                    <SelectItem key={opt.value} value={opt.value}>
                      <span className="flex items-center gap-2">
                        <Icon className="size-3.5" />
                        {opt.label}
                      </span>
                    </SelectItem>
                  );
                })}
              </SelectContent>
            </Select>
          </div>

          {/* Task */}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="spawn-task">Task</Label>
            <Textarea
              id="spawn-task"
              rows={3}
              placeholder="Describe what the agent should do…"
              value={task}
              onChange={(event) => setTask(event.target.value)}
            />
          </div>

          {/* Repository (optional) */}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="spawn-repo">
              Repository
              <span className="ml-1 text-xs font-normal text-muted-foreground">
                optional
              </span>
            </Label>
            <div className="flex gap-2">
              <Input
                id="spawn-repo"
                placeholder="owner/repo or full URL"
                value={repository}
                onChange={(event) => setRepository(event.target.value)}
                className="flex-1"
              />
              <Input
                placeholder="branch"
                value={repositoryRef}
                onChange={(event) => setRepositoryRef(event.target.value)}
                className="w-28"
              />
            </div>
          </div>

          {/* Model + effort */}
          <div className="flex gap-3">
            <div className="flex flex-1 flex-col gap-1.5">
              <Label htmlFor="spawn-model">
                Model
                <span className="ml-1 text-xs font-normal text-muted-foreground">
                  optional
                </span>
              </Label>
              <Input
                id="spawn-model"
                placeholder="e.g. claude-sonnet-4"
                value={model}
                onChange={(event) => setModel(event.target.value)}
              />
            </div>
            <div className="flex w-36 flex-col gap-1.5">
              <Label htmlFor="spawn-effort">Effort</Label>
              <Select value={effort} onValueChange={setEffort}>
                <SelectTrigger id="spawn-effort">
                  <SelectValue placeholder="Default" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="">Default</SelectItem>
                  {EFFORT_OPTIONS.map((opt) => (
                    <SelectItem key={opt.value} value={opt.value}>
                      {opt.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button disabled={!canSubmit} onClick={handleSubmit}>
            <Play className="size-3.5" aria-hidden="true" />
            {submitting ? "Spawning…" : "Spawn"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
