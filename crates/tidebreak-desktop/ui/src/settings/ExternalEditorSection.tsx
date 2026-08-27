import { useEffect, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  detectExternalEditors,
  type ExternalEditorProbe,
} from "@/code/codeWorktreeHost";
import {
  EXTERNAL_EDITORS,
  setEditorPreference,
  useEditorPreference,
  type ExternalEditorId,
} from "@/code/editorPreference";
import { SettingsField, SettingsSection } from "./primitives";

/**
 * Which editor "Open in editor" starts.
 *
 * The list is the same closed set the native side knows how to launch, and the
 * rows under it answer the doctor's question about editors: is this one on this
 * computer, and where. A reader whose editor is not on the list gives a program
 * path instead, which Tidebreak starts directly rather than through a shell.
 */
export function ExternalEditorSection({
  detect = detectExternalEditors,
  canDetect,
}: {
  /** Story and test seam for the native probe. */
  detect?: () => Promise<ExternalEditorProbe[]>;
  /** False on a window attached to another machine: no editors here to probe. */
  canDetect: boolean;
}) {
  const preference = useEditorPreference();
  const [probes, setProbes] = useState<ExternalEditorProbe[] | null>(null);

  useEffect(() => {
    if (!canDetect) return;
    let cancelled = false;
    void detect()
      .then((found) => {
        if (!cancelled) setProbes(found);
      })
      .catch(() => {
        if (!cancelled) setProbes([]);
      });
    return () => {
      cancelled = true;
    };
  }, [canDetect, detect]);

  const programs = new Map(
    (probes ?? []).map((probe) => [probe.id, probe.program]),
  );

  return (
    <SettingsSection
      title="External editor"
      description="Open a workspace file in the editor you already use, from the file viewer, the diff, or the workspace menu."
    >
      <SettingsField
        label="Editor"
        hint={editorHint(preference.editor, canDetect, probes, programs)}
      >
        <Select
          value={preference.editor}
          onValueChange={(next) =>
            setEditorPreference({
              ...preference,
              editor: next as ExternalEditorId,
            })
          }
        >
          <SelectTrigger aria-label="External editor">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {EXTERNAL_EDITORS.map((editor) => (
              <SelectItem key={editor.id} value={editor.id}>
                {editor.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingsField>
      {preference.editor === "custom" && (
        <SettingsField
          label="Program"
          hint="The full path to the program, such as /opt/homebrew/bin/nvim. Tidebreak starts it with the file path and no shell, so flags are not read."
        >
          <Input
            value={preference.customProgram}
            onChange={(event) =>
              setEditorPreference({
                ...preference,
                customProgram: event.target.value,
              })
            }
            placeholder="/opt/homebrew/bin/nvim"
            spellCheck={false}
          />
        </SettingsField>
      )}
      {canDetect && probes !== null && (
        <div className="divide-subtle overflow-hidden rounded-lg border divide-y">
          {EXTERNAL_EDITORS.filter((editor) => editor.id !== "custom").map(
            (editor) => {
              const program = programs.get(editor.id) ?? null;
              return (
                <div
                  key={editor.id}
                  className="bg-background flex items-center gap-3 px-3 py-2"
                >
                  <div className="flex min-w-0 flex-col gap-0.5">
                    <span className="truncate text-sm">{editor.label}</span>
                    {program ? (
                      <span
                        className="text-muted-foreground truncate font-mono text-2xs"
                        title={program}
                      >
                        {program}
                      </span>
                    ) : (
                      <span className="text-muted-foreground text-xs">
                        No launcher on this computer.
                      </span>
                    )}
                  </div>
                  <Badge
                    className="ml-auto shrink-0"
                    variant={program ? "success" : "outline"}
                  >
                    {program ? "Installed" : "Not found"}
                  </Badge>
                </div>
              );
            },
          )}
        </div>
      )}
    </SettingsSection>
  );
}

function editorHint(
  editor: ExternalEditorId,
  canDetect: boolean,
  probes: ExternalEditorProbe[] | null,
  programs: Map<string, string | null>,
): string {
  if (editor === "custom") return "Give the program below.";
  if (!canDetect) {
    return "This window works on another machine, so Tidebreak cannot check what is installed here.";
  }
  if (probes === null) return "Checking what is installed…";
  return programs.get(editor)
    ? "Installed and ready."
    : "Not found on this computer. Install its command-line launcher, or pick another editor.";
}
