import type { ApiClient } from "@/api/client";
import type { CodeRepoSnapshot } from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { RepositorySettings } from "./RepositorySettings";

/**
 * The repository-settings surface in the same dialog chrome Delivery uses for
 * tracked repos: title, scrollable body, Done.
 */
export function RepositorySettingsDialog({
  open,
  onOpenChange,
  client,
  repoId,
  repoLabel,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  client: Pick<
    ApiClient,
    "getCodeRepo" | "patchCodeRepo" | "getCodeRepoTrust" | "setCodeRepoTrust"
  >;
  repoId: string | null;
  repoLabel: string;
  onSaved?: (repo: CodeRepoSnapshot) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Repository settings</DialogTitle>
        </DialogHeader>
        <div className="max-h-[65vh] overflow-auto pr-1">
          <RepositorySettings
            client={client}
            repoId={repoId}
            repoLabel={repoLabel}
            onSaved={onSaved}
          />
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
