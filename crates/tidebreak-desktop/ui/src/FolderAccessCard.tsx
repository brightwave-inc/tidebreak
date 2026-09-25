import { useRef } from "react";

import type { PendingFolderAccessRequest } from "./api";
import type { FolderAccessDecision } from "./host";
import { ApprovalChoiceList } from "./ApprovalChoiceList";
import { AttentionCard } from "./AttentionCard";
import { Button } from "@/components/ui/button";

export function FolderAccessCard({
  request,
  nativeHost,
  nativeBusy,
  working,
  error,
  onDecision,
  onCancel,
}: {
  request: PendingFolderAccessRequest;
  nativeHost: boolean;
  nativeBusy: boolean;
  working: boolean;
  error: string | undefined;
  onDecision: (decision: FolderAccessDecision) => void;
  onCancel: () => void;
}) {
  const hint = request.folderHint
    ? request.folderHint[0].toUpperCase() + request.folderHint.slice(1)
    : null;
  const actionable =
    nativeHost && !nativeBusy && !request.claimedByDesktop && !working;
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  const choices = [
    { key: "allow", label: "Allow this folder" },
    { key: "decline", label: "Decline", muted: true },
  ];

  return (
    <AttentionCard
      title="Folder access requested"
      titleId={`folder-${request.callId}`}
      subtitle={request.reason}
      busy={working}
      error={error}
      headingRef={headingRef}
    >
      {hint && (
        <p className="text-muted-foreground text-sm break-words">
          Suggested starting location: <strong>{hint}</strong>
        </p>
      )}
      {working ? (
        <p
          className="text-muted-foreground text-sm"
          role="status"
          aria-live="polite"
        >
          Resolving the folder request…
        </p>
      ) : request.claimedByDesktop ? (
        <p
          className="text-muted-foreground text-sm"
          role="status"
          aria-live="polite"
        >
          This request is already being handled by the native desktop.
        </p>
      ) : !nativeHost ? (
        <>
          <p className="text-muted-foreground text-sm">
            Folder consent is unavailable in browser-only mode. This desktop
            cannot resolve conversations owned by the headless server.
          </p>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" size="sm" onClick={onCancel}>
              Cancel turn
            </Button>
          </div>
        </>
      ) : nativeBusy ? (
        <p
          className="text-muted-foreground text-sm"
          role="status"
          aria-live="polite"
        >
          Finish the current folder request first.
        </p>
      ) : (
        <ApprovalChoiceList
          disabled={!actionable}
          headingRef={headingRef}
          describedBy={`folder-${request.callId}`}
          onChoose={(index) =>
            onDecision(choices[index]!.key as FolderAccessDecision)
          }
          options={choices}
        />
      )}
    </AttentionCard>
  );
}
