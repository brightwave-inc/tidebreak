import { useState } from "react";
import { CircleCheck, TriangleAlert, Users } from "lucide-react";

import type { AppPublishResult, GatewayTeamInfo } from "@/api";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";

/**
 * Publishing one local app to a team, in three deliberate steps: pick the
 * team, read what publishing means, then do it.
 *
 * The middle step is the point of the dialog. Publishing is not a save — it
 * hands a running app to other people, and two things about it are invisible
 * from the app's page. The first is reach: from here on the gateway decides
 * who may open the app and runs its calls as *them*, so a teammate sees their
 * own data through it, not the author's. The second is the bundle. An app's
 * bundle is authored content, and whatever the author baked into it — a
 * copied spreadsheet, a customer list, notes pasted while building — ships
 * with the app and is visible to everyone who opens it. Nothing here can
 * enumerate that: the manifest describes what the app may *call*, never what
 * it already carries, so the only honest thing to do is say so before the
 * author publishes rather than after.
 *
 * A team that is disabled at the gateway is shown, greyed, rather than
 * dropped. An author looking for a team they know exists deserves to see that
 * it is switched off instead of wondering where it went.
 *
 * Publishing is deliberately not gated on the local grant — a grant is this
 * machine's consent to *run* the app here, while the gateway runs its own
 * consent gate for every viewer — so an app can be published without ever
 * having been opened on this machine. That is a real thing to know before
 * handing it to a team, so the preflight says it rather than letting the copy
 * imply the author has watched it work.
 */
export function AppPublishDialog({
  open,
  onOpenChange,
  appName,
  teams,
  grantedLocally,
  onPublish,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  appName: string;
  teams: GatewayTeamInfo[];
  /**
   * Whether a live local grant covers this app right now — the same verdict
   * the page's consent gate reads. False covers both an app never consented
   * to and one whose grant no longer holds, which is why the line it drives
   * speaks in the present tense rather than claiming a history.
   */
  grantedLocally: boolean;
  onPublish: (teamId: string) => Promise<AppPublishResult>;
}) {
  const [teamId, setTeamId] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [publishing, setPublishing] = useState(false);
  const [result, setResult] = useState<AppPublishResult | null>(null);
  // A thrown request failure, as distinct from an answer the gateway gave.
  const [failure, setFailure] = useState<string | null>(null);
  const selected = teams.find((team) => team.id === teamId) ?? null;

  function openAndReset(next: boolean) {
    if (!next && publishing) return;
    if (next) {
      setTeamId(null);
      setConfirming(false);
      setResult(null);
      setFailure(null);
    }
    onOpenChange(next);
  }

  async function publish() {
    if (!selected || publishing) return;
    setPublishing(true);
    setFailure(null);
    try {
      setResult(await onPublish(selected.id));
    } catch (caught) {
      setFailure(
        String(caught).replace(/^Error:\s*/, "").trim() ||
          "Could not publish this app.",
      );
    } finally {
      setPublishing(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={openAndReset}>
      <DialogContent
        className="max-w-md space-y-3"
        aria-busy={publishing}
        withCloseButton={!publishing}
      >
        <DialogHeader>
          <DialogTitle>Publish {appName}</DialogTitle>
          <DialogDescription>
            {result || failure
              ? "Your model gateway's answer."
              : confirming
                ? "What publishing does, before you do it."
                : "Choose the team that should be able to open this app."}
          </DialogDescription>
        </DialogHeader>

        {result || failure ? (
          <PublishAnswer
            appName={appName}
            teamName={selected?.name ?? "your team"}
            result={result}
            failure={failure}
          />
        ) : confirming && selected ? (
          <div className="flex flex-col gap-3 text-sm">
            <p>
              Everyone in <span className="font-medium">{selected.name}</span>{" "}
              will be able to open{" "}
              <span className="font-medium">{appName}</span> through your model
              gateway. It runs as whoever opens it, using their own access —
              not yours.
            </p>
            <p
              className="border-warning/50 text-warning flex items-start gap-2 rounded-md border px-3 py-2 text-xs"
              role="note"
            >
              <TriangleAlert
                className="mt-0.5 size-3.5 shrink-0"
                aria-hidden="true"
              />
              <span>
                Anything built into this app travels with it. Data the author
                pasted or embedded while making it is part of the app and will
                be visible to everyone in the team — this page cannot list what
                that is.
              </span>
            </p>
            {!grantedLocally && (
              <p className="text-muted-foreground text-xs">
                This app isn&rsquo;t currently allowed to run on this machine,
                so you may never have seen it work.
              </p>
            )}
            <p className="text-muted-foreground text-xs">
              The team receives this app&rsquo;s current revision. Later edits
              are not published until you publish again.
            </p>
          </div>
        ) : (
          <TeamPicker
            teams={teams}
            teamId={teamId}
            disabled={publishing}
            onSelect={setTeamId}
          />
        )}

        <div className="flex justify-end gap-2">
          {result || failure ? (
            <Button size="sm" onClick={() => openAndReset(false)}>
              Done
            </Button>
          ) : confirming ? (
            <>
              <Button
                variant="outline"
                size="sm"
                disabled={publishing}
                onClick={() => setConfirming(false)}
              >
                Back
              </Button>
              <Button
                size="sm"
                disabled={publishing || !selected}
                onClick={() => void publish()}
              >
                <Users className="size-3.5" aria-hidden="true" />
                {publishing ? "Publishing…" : `Publish to ${selected?.name}`}
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              disabled={!selected?.enabled}
              onClick={() => setConfirming(true)}
            >
              Continue
            </Button>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function TeamPicker({
  teams,
  teamId,
  disabled,
  onSelect,
}: {
  teams: GatewayTeamInfo[];
  teamId: string | null;
  disabled: boolean;
  onSelect: (teamId: string) => void;
}) {
  if (teams.length === 0) {
    return (
      <p className="text-muted-foreground text-sm" role="status">
        You are not a member of any team at your model gateway, so there is
        nobody to publish this app to yet.
      </p>
    );
  }
  return (
    <RadioGroup
      value={teamId ?? ""}
      onValueChange={onSelect}
      aria-label="Team to publish to"
      disabled={disabled}
      className="gap-1"
    >
      {teams.map((team) => {
        const id = `publish-team-${team.id}`;
        return (
          <label
            key={team.id}
            htmlFor={id}
            className={
              team.enabled
                ? "hover:bg-muted flex w-full cursor-pointer items-center gap-2 rounded-md p-2 text-left"
                : "flex w-full items-center gap-2 rounded-md p-2 text-left opacity-60"
            }
          >
            <RadioGroupItem id={id} value={team.id} disabled={!team.enabled} />
            <span className="min-w-0 flex-1 truncate text-sm">{team.name}</span>
            {!team.enabled && (
              <span className="text-muted-foreground text-xs">
                disabled at your gateway
              </span>
            )}
          </label>
        );
      })}
    </RadioGroup>
  );
}

/**
 * What came back. A refusal is rendered in the gateway's own words wherever it
 * gave any — a bundle refused for calling host-local bridge verbs names those
 * verbs, and this page could not reconstruct that list if it tried.
 */
function PublishAnswer({
  appName,
  teamName,
  result,
  failure,
}: {
  appName: string;
  teamName: string;
  result: AppPublishResult | null;
  failure: string | null;
}) {
  if (failure) {
    return <Refusal heading="Could not publish this app." detail={failure} />;
  }
  if (!result) return null;
  switch (result.outcome) {
    case "published":
      return (
        <p
          className="text-success flex items-start gap-2 text-sm"
          role="status"
        >
          <CircleCheck className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          <span>
            {appName} is now available to {teamName}. They can open it from your
            model gateway.
          </span>
        </p>
      );
    case "no_gateway":
      return (
        <Refusal heading="This profile is not paired with a model gateway, so there is nowhere to publish." />
      );
    case "not_registered":
      return (
        <Refusal
          heading="Your model gateway is not holding this app yet."
          detail="Nothing was published. Try again in a moment — publishing registers the app as it goes."
        />
      );
    case "not_supported":
      return (
        <Refusal
          heading="Your model gateway did not accept this publish."
          detail="It may be running a version that cannot publish apps, or this app or team may no longer be yours to publish to."
        />
      );
    case "app_disabled":
      return (
        <Refusal
          heading="This app is switched off at your model gateway."
          detail={result.message}
        />
      );
    case "unreachable":
      return (
        <Refusal
          heading="Could not reach your model gateway."
          detail={result.message}
        />
      );
    case "refused":
      return (
        <Refusal
          heading="Your model gateway refused this publish."
          detail={result.message}
        />
      );
  }
}

function Refusal({ heading, detail }: { heading: string; detail?: string }) {
  return (
    <div className="flex items-start gap-2 text-sm" role="alert">
      <TriangleAlert
        className="text-warning mt-0.5 size-4 shrink-0"
        aria-hidden="true"
      />
      <span className="min-w-0 flex-1">
        {heading}
        {detail && (
          <span className="text-muted-foreground mt-1 block text-xs">
            {detail}
          </span>
        )}
      </span>
    </div>
  );
}
