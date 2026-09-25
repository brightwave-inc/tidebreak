import {
  CircleAlert,
  CircleCheck,
  Download,
  ExternalLink,
  RefreshCw,
  X,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { openInBrowser } from "@/openInBrowser";

const RELEASES_URL = "https://github.com/naingthet/tidebreak/releases";

type UpdateReadyCardProps =
  | {
      status: "checking" | "downloading";
      version: string | null;
      onDismiss: () => void;
    }
  | {
      /** A newer release is published and not downloaded yet. */
      status: "available";
      version: string | null;
      /** Why the last download failed, so you can fix it and try again. */
      error?: string | null;
      onDownload: () => void;
      onDismiss: () => void;
    }
  | {
      /** The check you asked for found nothing newer. */
      status: "up-to-date";
      /** The version you are running. */
      version: string | null;
      onDismiss: () => void;
    }
  | {
      /** The check you asked for failed; `message` says why. */
      status: "failed";
      message: string;
      onDismiss: () => void;
    }
  | {
      status?: "ready";
      version: string | null;
      /** Why the last restart, or the download of a newer release, failed. */
      error?: string | null;
      onRestart: () => void;
      onDismiss: () => void;
    };

export function releaseNotesUrl(version: string | null): string {
  const normalized = version?.trim().replace(/^v/, "");
  return normalized
    ? `${RELEASES_URL}/tag/v${encodeURIComponent(normalized)}`
    : `${RELEASES_URL}/latest`;
}

/**
 * Split an update error into a title and its detail. Update errors say what
 * failed in their first sentence and why, or what to do, in the rest:
 * "Could not check for updates. Check your internet connection…".
 */
export function splitUpdateMessage(message: string): {
  title: string;
  detail: string | null;
} {
  const match = /^(.+?)[.!?]\s+(\S[\s\S]*)$/.exec(message.trim());
  if (!match) {
    return { title: message.trim().replace(/[.!?]$/, ""), detail: null };
  }
  return { title: match[1], detail: match[2] };
}

function cardCopy(props: UpdateReadyCardProps): {
  title: string;
  description: string | null;
} {
  switch (props.status) {
    case "checking":
      return {
        title: "Checking for updates",
        description: "Looking for a newer version of Tidebreak…",
      };
    case "downloading":
      return {
        title: "Downloading update",
        description: props.version
          ? `Downloading and verifying Tidebreak ${props.version}…`
          : "Downloading and verifying the update…",
      };
    case "available":
      return {
        title: "Update available",
        description: props.version
          ? `Tidebreak ${props.version} is available.`
          : "A new version of Tidebreak is available.",
      };
    case "up-to-date":
      return {
        title: "You're up to date",
        description: props.version
          ? `Tidebreak ${props.version} is the latest version.`
          : "You have the latest version of Tidebreak.",
      };
    case "failed": {
      const { title, detail } = splitUpdateMessage(props.message);
      return { title, description: detail };
    }
    default:
      return {
        title: "Update ready",
        description: props.version
          ? `Tidebreak ${props.version} is downloaded and ready to install.`
          : "A Tidebreak update is downloaded and ready to install.",
      };
  }
}

function CardIcon({ status }: { status: UpdateReadyCardProps["status"] }) {
  switch (status) {
    case "checking":
    case "downloading":
      return <Spinner className="mt-0.5 size-4" aria-hidden="true" />;
    case "available":
      return (
        <Download
          className="mt-0.5 size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
      );
    case "up-to-date":
      return (
        <CircleCheck
          className="mt-0.5 size-4 shrink-0 text-success"
          aria-hidden="true"
        />
      );
    case "failed":
      return (
        <CircleAlert
          className="mt-0.5 size-4 shrink-0 text-critical-foreground"
          aria-hidden="true"
        />
      );
    default:
      return (
        <RefreshCw
          className="mt-0.5 size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
      );
  }
}

export function UpdateReadyCard(props: UpdateReadyCardProps) {
  const { onDismiss } = props;
  const status = props.status ?? "ready";
  const { title, description } = cardCopy(props);
  const offersRelease =
    props.status === "available" ||
    props.status === "ready" ||
    props.status === undefined;
  const releaseVersion = offersRelease ? props.version : null;
  const error = offersRelease ? props.error : null;

  return (
    <aside
      className="relative rounded-xl border border-border bg-popover p-4 text-popover-foreground shadow-lg"
      aria-label={title}
      aria-live={status === "ready" ? undefined : "polite"}
    >
      <button
        type="button"
        className="absolute top-2.5 right-2.5 grid size-7 cursor-pointer place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/25"
        aria-label="Dismiss update notice"
        onClick={onDismiss}
      >
        <X className="size-4" aria-hidden="true" />
      </button>

      <div className="flex items-start gap-3 pr-7">
        <CardIcon status={status} />
        <div className="min-w-0">
          <p className="text-md font-semibold">{title}</p>
          {description && (
            <p className="mt-1 text-sm text-muted-foreground">{description}</p>
          )}
          {error && (
            <p
              className="mt-2 text-sm break-words text-critical-foreground"
              role="alert"
            >
              {error}
            </p>
          )}
        </div>
      </div>

      {offersRelease && (
        <div className="mt-4 flex flex-wrap items-center gap-2">
          {props.status === "available" ? (
            <Button type="button" size="sm" onClick={props.onDownload}>
              Download
            </Button>
          ) : (
            <Button type="button" size="sm" onClick={props.onRestart}>
              Restart and update
            </Button>
          )}
          <Button
            type="button"
            size="sm"
            variant="ghost"
            onClick={() => void openInBrowser(releaseNotesUrl(releaseVersion))}
          >
            Release notes
            <ExternalLink aria-hidden="true" />
          </Button>
        </div>
      )}
    </aside>
  );
}
