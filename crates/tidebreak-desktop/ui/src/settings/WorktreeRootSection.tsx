import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SettingsField, SettingsSection } from "./primitives";

/**
 * Where new code workspaces put their worktrees.
 *
 * A worktree holds uncommitted work on a real branch, so this path is the
 * user's, not the app's — the field leads with the folder that is in force and
 * says plainly what changing it does and does not do. Presentational: the
 * panel owns loading, saving, and the native picker.
 */
export function WorktreeRootSection({
  value,
  effectiveRoot,
  defaultRoot,
  inherited,
  busy,
  canBrowse,
  onChange,
  onBrowse,
  onSave,
  onReset,
}: {
  /** The draft in the field, which may differ from what is saved. */
  value: string;
  /** The root the next workspace uses right now. */
  effectiveRoot: string;
  /** What resetting returns to. */
  defaultRoot: string;
  /** True while no root is stored and the default is in force. */
  inherited: boolean;
  busy: boolean;
  canBrowse: boolean;
  onChange: (value: string) => void;
  onBrowse: () => void;
  onSave: () => void;
  onReset: () => void;
}) {
  const dirty = value.trim() !== (inherited ? "" : effectiveRoot);
  return (
    <SettingsSection
      title="Workspace folder"
      description="Every new workspace gets a git worktree under this folder. Workspaces that already exist keep the folder they were created in."
    >
      <SettingsField
        label="Folder"
        hint={
          inherited
            ? `Using the default, ${defaultRoot}.`
            : `Reset to use the default, ${defaultRoot}.`
        }
      >
        <div className="flex gap-2">
          <Input
            value={value}
            onChange={(event) => onChange(event.target.value)}
            placeholder={defaultRoot}
            disabled={busy}
            spellCheck={false}
          />
          {canBrowse && (
            <Button
              type="button"
              variant="outline"
              onClick={onBrowse}
              disabled={busy}
            >
              Browse
            </Button>
          )}
        </div>
      </SettingsField>
      <div className="flex gap-2">
        <Button
          type="button"
          onClick={onSave}
          disabled={busy || !dirty || !value.trim()}
        >
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button
          type="button"
          variant="ghost"
          onClick={onReset}
          disabled={busy || inherited}
        >
          Reset to default
        </Button>
      </div>
    </SettingsSection>
  );
}
