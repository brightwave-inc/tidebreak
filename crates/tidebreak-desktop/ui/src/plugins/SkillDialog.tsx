import { useEffect, useState } from "react";
import { Sparkles } from "lucide-react";

import type { PluginSkillInfo } from "@/api";
import { MessageMarkdown } from "@/MessageMarkdown";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";

/**
 * One skill, up close: its identity, its own switch, and the instruction body
 * it stages — the exact markdown the model is taught, shown to the reader
 * verbatim rather than summarized.
 *
 * The body is fetched when the dialog opens, not carried in the catalog: a
 * body is kilobytes where a catalog row is bytes.
 */
export function SkillDialog({
  skill,
  gated,
  onOpenChange,
  onToggle,
  loadInstructions,
}: {
  /** The skill showing, or `null` while the dialog is closed. */
  skill: PluginSkillInfo | null;
  /** True while an owning bundle is off, which gates the switch here too. */
  gated: boolean;
  onOpenChange: (open: boolean) => void;
  onToggle: (skill: PluginSkillInfo, enabled: boolean) => void;
  loadInstructions: (name: string) => Promise<{ instructions: string }>;
}) {
  const name = skill?.name ?? null;
  const [body, setBody] = useState<
    | { state: "loading" }
    | { state: "failed" }
    | { state: "ready"; text: string }
  >({ state: "loading" });

  useEffect(() => {
    if (!name) return;
    let cancelled = false;
    setBody({ state: "loading" });
    loadInstructions(name).then(
      ({ instructions }) => {
        if (!cancelled) setBody({ state: "ready", text: instructions });
      },
      () => {
        if (!cancelled) setBody({ state: "failed" });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [name, loadInstructions]);

  return (
    <Dialog open={skill !== null} onOpenChange={onOpenChange}>
      {skill && (
        <DialogContent className="flex max-h-[85vh] max-w-2xl flex-col gap-4">
          {/* Vertically centered on the close button's size-10 hit area, with a
              comfortable gap to its left. */}
          <div className="absolute top-4 right-14">
            <Switch
              aria-label={`Enable ${skill.name}`}
              checked={skill.enabled}
              disabled={gated}
              onCheckedChange={(enabled) => onToggle(skill, enabled)}
            />
          </div>

          <div className="flex flex-col gap-2 pr-28">
            <div className="text-icon-amber grid size-10 place-items-center">
              <Sparkles className="size-5" aria-hidden="true" />
            </div>
            <DialogTitle className="flex items-baseline gap-2">
              {skill.name}
              <span className="text-muted-foreground text-base font-normal">
                Skill
              </span>
            </DialogTitle>
            <DialogDescription>{skill.description}</DialogDescription>
          </div>

          <div className="bg-muted/50 min-h-0 flex-1 overflow-y-auto rounded-lg border p-4">
            {body.state === "loading" && (
              <div
                className="flex flex-col gap-2"
                role="status"
                aria-label="Loading skill"
              >
                <Skeleton className="h-4 w-3/4" />
                <Skeleton className="h-4 w-full" />
                <Skeleton className="h-4 w-2/3" />
              </div>
            )}
            {body.state === "failed" && (
              <p className="text-sm" role="alert">
                Could not load this skill's instructions.
              </p>
            )}
            {body.state === "ready" && (
              <div className="[&_.message-markdown]:text-sm">
                <MessageMarkdown>{body.text}</MessageMarkdown>
              </div>
            )}
          </div>

          <p className="text-muted-foreground text-xs">
            {skill.origin === "user"
              ? "Installed by you, from your data directory."
              : "Bundled with Tidebreak."}
            {gated && " Its plugin is off, so the skill cannot run right now."}
          </p>
        </DialogContent>
      )}
    </Dialog>
  );
}
