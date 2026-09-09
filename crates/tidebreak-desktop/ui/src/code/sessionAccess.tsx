import { useEffect, useState } from "react";
import { EyeOff, Share2, UserRound, Users } from "lucide-react";

import type {
  CodeSessionSnapshot,
  SessionAccessLevel,
  SessionAccessSummary,
  SessionAllowedAction,
  SessionVisibility,
} from "../api/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { WithTooltip } from "@/components/ui/tooltip";
import { friendlyErrorMessage } from "@/lib/utils";
import type { CodeSessionAccess } from "./useCodeSessionAccess";

/** True only for an action the server-resolved summary actually grants. */
export function summaryAllows(
  summary: SessionAccessSummary | null | undefined,
  action: SessionAllowedAction,
): boolean {
  return summary?.allowed_actions.includes(action) ?? false;
}

/** The canonical read-only composer explanation for a viewer. */
export function ReadOnlyComposerExplanation({
  summary,
}: {
  summary: SessionAccessSummary;
}) {
  const origin = summary.session.external_origin;
  return (
    <div
      className="border-border-subtle bg-background/70 mx-auto mt-3 flex w-[calc(100%-2rem)] max-w-3xl items-start gap-2 rounded-lg border px-3 py-2"
      data-testid="read-only-composer-explanation"
    >
      <EyeOff
        className="text-muted-foreground mt-px size-3.5 shrink-0"
        aria-hidden
      />
      <p className="text-muted-foreground text-xs">
        You can read this session, but only{" "}
        <span className="font-medium text-foreground">
          {summary.owner_principal}
        </span>
        {origin
          ? ` — who started it from ${origin.channel_kind} — can continue it.`
          : " — who started it — can continue it."}{" "}
        Ask the owner to make you a contributor if you need to send a message.
      </p>
    </div>
  );
}

/** A pending approval a viewer may see but cannot decide. */
export function UndecidableApproval({
  approvalId,
}: {
  approvalId: string;
}) {
  return (
    <section
      className="bg-background max-w-prose border p-4 text-left text-xs"
      data-testid="undecidable-approval"
      aria-label="Approval needs the owner"
    >
      <p className="font-medium text-foreground">
        This needs a decision from the session owner.
      </p>
      <p className="text-muted-foreground mt-1">
        Your role lets you read this session, not decide it. Ask the owner or a
        contributor to approve or deny it.
      </p>
      <span className="sr-only">{approvalId}</span>
    </section>
  );
}

/**
 * Participants on the session header: the safe owner identity and the channel
 * binding when one exists. Only the owner sees the full access rows; everyone
 * else gets the same safe owner/channel line.
 */
export function SessionParticipants({
  summary,
  session,
}: {
  summary: SessionAccessSummary;
  session: CodeSessionSnapshot;
}) {
  const origin = session.external_origin;
  return (
    <span
      className="text-muted-foreground inline-flex min-w-0 items-center gap-1.5 text-xs"
      data-testid="session-participants"
      title={`Owner: ${summary.owner_principal}${
        origin ? ` · ${origin.channel_kind} conversation` : ""
      }`}
    >
      <UserRound className="size-3.5 shrink-0" aria-hidden />
      <span className="truncate">{summary.owner_principal}</span>
      {origin && (
        <span className="flex min-w-0 items-center gap-1">
          <span aria-hidden>·</span>
          <Users className="size-3.5 shrink-0" aria-hidden />
          <span className="truncate">{origin.channel_kind} conversation</span>
        </span>
      )}
    </span>
  );
}

const VISIBILITY_LABELS: Record<SessionVisibility, string> = {
  private: "Private — only people you share it with",
  deployment: "Deployment — anyone signed in to this machine",
};

/** Owner-only Share control: visibility plus principal viewer/contributor rows. */
export function SessionShareControl({
  access,
  session,
}: {
  access: CodeSessionAccess;
  session: CodeSessionSnapshot;
}) {
  const [open, setOpen] = useState(false);
  const [subject, setSubject] = useState("");
  const [level, setLevel] = useState<SessionAccessLevel>("view");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) {
      setSubject("");
      setError(null);
    }
  }, [open]);

  async function toggleVisibility(next: SessionVisibility) {
    setWorking(true);
    setError(null);
    try {
      await access.setVisibility(next);
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not change visibility"));
    } finally {
      setWorking(false);
    }
  }

  async function add() {
    const trimmed = subject.trim();
    if (!trimmed.startsWith("principal:")) {
      setError("Enter a principal subject, for example principal:user:sam");
      return;
    }
    setWorking(true);
    setError(null);
    try {
      await access.addAccess(trimmed, level);
      setSubject("");
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not share this session"));
    } finally {
      setWorking(false);
    }
  }

  async function revoke(subjectToRevoke: string) {
    setWorking(true);
    setError(null);
    try {
      await access.revokeAccess(subjectToRevoke);
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not remove that grant"));
    } finally {
      setWorking(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          className="rounded-lg"
          aria-label="Share session"
          data-testid="share-session-trigger"
        >
          <Share2 />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Share this session</DialogTitle>
          <DialogDescription>
            The owner controls who can read and who can contribute. Visibility
            lets every signed-in person on this machine read without a row.
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <label className="flex items-center justify-between gap-3 text-sm">
            <span className="flex flex-col gap-0.5">
              <span className="font-medium">Deployment visibility</span>
              <span className="text-xs text-muted-foreground">
                {VISIBILITY_LABELS[session.visibility]}
              </span>
            </span>
            <Switch
              checked={session.visibility === "deployment"}
              disabled={working}
              onCheckedChange={(checked) =>
                void toggleVisibility(checked ? "deployment" : "private")
              }
              aria-label="Deployment visibility"
            />
          </label>
          <div className="flex flex-col gap-2">
            <label className="text-xs font-medium" htmlFor="share-subject">
              Principal
            </label>
            <div className="flex gap-2">
              <Input
                id="share-subject"
                value={subject}
                onChange={(event) => setSubject(event.target.value)}
                placeholder="principal:user:sam"
                className="flex-1"
              />
              <Select
                value={level}
                onValueChange={(value) =>
                  setLevel(value as SessionAccessLevel)
                }
              >
                <SelectTrigger className="w-32">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="view">Viewer</SelectItem>
                  <SelectItem value="contribute">Contributor</SelectItem>
                </SelectContent>
              </Select>
              <Button
                type="button"
                size="sm"
                disabled={working || subject.trim().length === 0}
                onClick={() => void add()}
              >
                Add
              </Button>
            </div>
            {error && <p className="text-xs text-destructive">{error}</p>}
          </div>
        </div>
        {access.rows && (
          <div className="flex flex-col gap-1.5">
            <label className="text-xs font-medium">Shared with</label>
            {access.rows.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                No one has access yet. Add a viewer or contributor above.
              </p>
            ) : (
              access.rows.map((row) => (
                <div
                  key={row.subject}
                  className="border-border-subtle flex items-center justify-between gap-2 border-t px-0.5 py-1.5 text-xs"
                >
                  <span className="min-w-0 truncate font-mono">
                    {row.subject}
                  </span>
                  <span className="shrink-0 text-muted-foreground">
                    {row.level === "contribute" ? "Contributor" : "Viewer"}
                  </span>
                  <Button
                    type="button"
                    variant="ghost"
                    size="xs"
                    disabled={working}
                    onClick={() => void revoke(row.subject)}
                  >
                    Remove
                  </Button>
                </div>
              ))
            )}
          </div>
        )}
        <DialogFooter />
      </DialogContent>
    </Dialog>
  );
}
