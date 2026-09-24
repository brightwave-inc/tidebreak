import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { Button } from "@/components/ui/button";
import {
  cliCommandHost,
  type CliCommandChange,
  type CliCommandHost,
  type CliCommandStatus,
  type CliLink,
  type CliLocation,
} from "@/cliCommand";
import {
  SettingsError,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
  type SettingsStatusTone,
} from "./primitives";

/** The line a shell profile needs when `~/.local/bin` is not on the PATH. */
export const LOCAL_BIN_PATH_LINE = 'export PATH="$HOME/.local/bin:$PATH"';

type Available = Extract<CliCommandStatus, { status: "available" }>;

/**
 * The home folder, read off the account's own link, which always sits at
 * `<home>/.local/bin/tidebreak`.
 */
function homeOf(status: Available): string | null {
  const home = status.user.path.replace(/\/\.local\/bin\/tidebreak$/, "");
  return home && home !== status.user.path ? home : null;
}

/** A path with the home folder written as `~`, the way a terminal shows it. */
function homeRelative(path: string, status: Available): string {
  const home = homeOf(status);
  return home && path.startsWith(`${home}/`)
    ? `~${path.slice(home.length)}`
    : path;
}

/** The folder a link sits in. */
function folderOf(path: string): string {
  return path.replace(/\/[^/]*$/, "");
}

/** The line that puts a link's folder on the PATH, for a shell profile. */
function pathLine(link: CliLink, status: Available): string {
  const folder = folderOf(link.path);
  return folder === folderOf(status.user.path)
    ? LOCAL_BIN_PATH_LINE
    : `export PATH="${folder}:$PATH"`;
}

/**
 * A path in the machine's voice. It wraps only where it has to, so a path
 * that fits on the next line moves there whole.
 */
function Path({ children }: { children: string }) {
  return <code className="wrap-anywhere font-mono text-xs">{children}</code>;
}

type Verdict = {
  tone: SettingsStatusTone;
  label: string;
  description: ReactNode;
  /** The line to add to a shell profile, when a terminal would not look. */
  pathLine?: string;
};

/**
 * The verdict the page leads with.
 *
 * What a new terminal runs is judged by where it leads, not by its path:
 * Tidebreak's link in either folder runs this app, so a PATH that finds the
 * link for all users first still reads as installed. The link for all users
 * gets the same checks as the one for this account.
 */
export function commandLineVerdict(
  status: CliCommandStatus | null,
  native: boolean,
): Verdict | null {
  if (!native) {
    return {
      tone: "neutral",
      label: "Not available here",
      description:
        "Open Tidebreak on your Mac to install the tidebreak command.",
    };
  }
  if (!status) return null;
  if (status.status === "unavailable") {
    switch (status.reason) {
      case "unsupported":
        return {
          tone: "neutral",
          label: "Not available here",
          description:
            "The macOS app installs the tidebreak command. On other systems, run the tidebreak binary that ships with the package.",
        };
      case "not_bundled":
        return {
          tone: "neutral",
          label: "No command in this build",
          description:
            "This copy of Tidebreak has no tidebreak command to install. The app from the download page includes it.",
        };
      case "temporary_location":
        return {
          tone: "warning",
          label: "Move Tidebreak to Applications first",
          description:
            "Tidebreak is running from a disk image or a temporary copy, so a link to it would stop working. Move it to your Applications folder, open it from there, and install the command.",
        };
    }
  }
  const { user, system, resolved } = status;
  const userPath = <Path>{homeRelative(user.path, status)}</Path>;
  if (user.state === "foreign") {
    return {
      tone: "warning",
      label: "Another tidebreak is in the way",
      description: (
        <>
          Tidebreak did not make {userPath}, so it will not replace it. Move or
          delete it, then install the command.
        </>
      ),
    };
  }
  if (user.state === "stale") {
    return {
      tone: "warning",
      label: "Points to another copy of Tidebreak",
      description: (
        <>
          {userPath} opens <Path>{homeRelative(user.target, status)}</Path>,
          which is not this app. Repair the command to point it here.
        </>
      ),
    };
  }

  // The link a new terminal should find: this account's, or else the one
  // for all users.
  const forAll = user.state !== "installed";
  const link = forAll ? system : user;
  if (link.state !== "installed") {
    return {
      tone: "neutral",
      label: "Not installed",
      description: (
        <>
          Install the command to run Tidebreak from a terminal. It links{" "}
          {userPath} to this app and creates the folder if needed.
        </>
      ),
    };
  }
  const linkPath = <Path>{homeRelative(link.path, status)}</Path>;
  const folder = <Path>{homeRelative(folderOf(link.path), status)}</Path>;
  const ready: Verdict = {
    tone: "ready",
    label: forAll ? "Installed for all users" : "Installed",
    description: forAll ? (
      <>
        Run <Path>tidebreak</Path> in a new terminal. It links to this app from{" "}
        {linkPath}.
      </>
    ) : (
      <>
        Run <Path>tidebreak</Path> in a new terminal. It links to this app, so
        updates keep it current.
      </>
    ),
  };
  if (resolved?.thisApp) return ready;
  if (link.onPath === false) {
    return {
      tone: "warning",
      label: forAll
        ? "Installed for all users, but not on your PATH"
        : "Installed, but not on your PATH",
      description: (
        <>
          The command is at {linkPath}, but a new terminal does not look in that
          folder. Add the line below to your shell profile, such as{" "}
          <Path>~/.zshrc</Path>, then open a new terminal.
        </>
      ),
      pathLine: pathLine(link, status),
    };
  }
  if (resolved) {
    return {
      tone: "warning",
      label: "Another tidebreak runs first",
      description: (
        <>
          A new terminal runs <Path>{homeRelative(resolved.path, status)}</Path>{" "}
          before {linkPath}. Remove it, or put {folder} earlier in your PATH.
        </>
      ),
    };
  }
  // The PATH could not be read, so there is nothing to say against it.
  return ready;
}

/** What an install or uninstall did, in one sentence. */
export function changeSummary(change: CliCommandChange): ReactNode {
  if (change.status.status !== "available") return null;
  const link =
    change.location === "user" ? change.status.user : change.status.system;
  const shown = homeRelative(link.path, change.status);
  const path = <Path>{shown}</Path>;
  switch (change.outcome) {
    case "created":
      return change.folderCreated ? (
        <>
          Created {<Path>{folderOf(shown)}</Path>} and linked the command there.
        </>
      ) : (
        <>Linked {path} to this app.</>
      );
    case "updated":
      return <>Pointed {path} at this app.</>;
    case "unchanged":
      return <>{path} already links to this app.</>;
    case "removed":
      return <>Removed {path}.</>;
    case "absent":
      return <>{path} was already gone.</>;
    case "cancelled":
      return "You cancelled the administrator prompt, so nothing changed.";
  }
}

type Work = `${"install" | "uninstall"}:${CliLocation}` | "check";

/**
 * What the last action said, shown beside the action: a change under the
 * section that made it, and a failed status read under the verdict.
 */
type Outcome = {
  location: CliLocation | null;
  text: ReactNode;
  failed: boolean;
};

/**
 * Settings → Command line: installs the `tidebreak` command that ships inside
 * the app, the way editors install their launcher.
 *
 * The link goes in `~/.local/bin` by default, and the page says whether a new
 * terminal looks there. `/usr/local/bin` is offered only through the macOS
 * administrator prompt. Tidebreak never replaces a `tidebreak` it did not
 * make, repairs a link to another copy of the app, and uninstalls only its
 * own link.
 *
 * The app menu's Install the tidebreak Command sets `installRequested`. Each
 * time it turns on, the page runs the install once and calls
 * `onInstallRequestTaken`, which clears it, so the next choice of the menu
 * item turns it on again. That works whether the page just opened or was
 * already open, and the result lands on the page that explains it.
 */
export function CommandLinePanel({
  host = cliCommandHost,
  installRequested = false,
  onInstallRequestTaken,
}: {
  host?: CliCommandHost;
  installRequested?: boolean;
  onInstallRequestTaken?: () => void;
}) {
  const native = host.available();
  const [status, setStatus] = useState<CliCommandStatus | null>(null);
  const [work, setWork] = useState<Work | null>(native ? "check" : null);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [installPending, setInstallPending] = useState(false);
  const mounted = useRef(true);
  // Each read or change takes a ticket, and only the latest one's answer
  // lands, so a status read that started before a change never overwrites
  // what the change reported.
  const latest = useRef(0);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const current = useCallback(
    (ticket: number) => mounted.current && ticket === latest.current,
    [],
  );

  const check = useCallback(async () => {
    if (!native) return;
    const ticket = ++latest.current;
    setWork("check");
    setOutcome(null);
    try {
      const next = await host.status();
      if (current(ticket)) setStatus(next);
    } catch (err) {
      if (current(ticket)) {
        setOutcome({ location: null, text: String(err), failed: true });
      }
    } finally {
      if (current(ticket)) setWork(null);
    }
  }, [current, host, native]);

  const change = useCallback(
    async (action: "install" | "uninstall", location: CliLocation) => {
      const ticket = ++latest.current;
      setWork(`${action}:${location}`);
      setOutcome(null);
      try {
        const result = await host[action](location);
        if (!current(ticket)) return;
        setStatus(result.status);
        const text = changeSummary(result);
        setOutcome(text ? { location, text, failed: false } : null);
      } catch (err) {
        if (!current(ticket)) return;
        setOutcome({ location, text: String(err), failed: true });
        // A refusal says why; the state behind it is worth reading afresh.
        void host
          .status()
          .then((next) => current(ticket) && setStatus(next))
          .catch(() => undefined);
      } finally {
        if (current(ticket)) setWork(null);
      }
    },
    [current, host],
  );

  // The menu's request: taken once each time it turns on, and handed back so
  // the page that sent it can clear it.
  const takenRequest = useRef(false);
  useEffect(() => {
    if (!installRequested) {
      takenRequest.current = false;
      return;
    }
    if (takenRequest.current) return;
    takenRequest.current = true;
    onInstallRequestTaken?.();
    if (native) setInstallPending(true);
  }, [installRequested, native, onInstallRequestTaken]);

  // Read the status on open, unless the menu's install is about to report it.
  const opened = useRef(false);
  useEffect(() => {
    if (opened.current) return;
    opened.current = true;
    if (!(installRequested && native)) void check();
  }, [check, installRequested, native]);

  // Run the requested install once no other change is under way. It takes
  // over from a status read, whose answer it replaces.
  const changing = work !== null && work !== "check";
  useEffect(() => {
    if (!installPending || changing) return;
    setInstallPending(false);
    void change("install", "user");
  }, [installPending, changing, change]);

  const verdict = commandLineVerdict(status, native);
  const available = status?.status === "available" ? status : null;
  const busy = work !== null || installPending;
  const outcomeFor = (location: CliLocation | null) =>
    outcome?.location === location ? <OutcomeLine outcome={outcome} /> : null;

  return (
    <SettingsPanel
      title="Command line"
      description="Run Tidebreak from a terminal with the tidebreak command that ships inside the app."
      busy={busy}
    >
      <SettingsSection
        title="The tidebreak command"
        description="Installs for your account, without a password."
      >
        {verdict ? (
          <SettingsStatus
            tone={verdict.tone}
            label={verdict.label}
            description={verdict.description}
          />
        ) : (
          <p role="status" className="text-sm text-muted-foreground">
            Checking the tidebreak command…
          </p>
        )}
        {verdict?.pathLine && (
          // Its own line, so one click selects exactly what to paste.
          <code
            aria-label="Line to add to your shell profile"
            className="block rounded-md bg-muted px-3 py-2 font-mono text-xs select-all wrap-anywhere"
          >
            {verdict.pathLine}
          </code>
        )}
        {outcomeFor(null)}
        {available && (
          <div className="flex flex-wrap gap-2">
            <UserActions
              link={available.user}
              work={work}
              busy={busy}
              onInstall={() => void change("install", "user")}
              onUninstall={() => void change("uninstall", "user")}
            />
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => void check()}
            >
              {work === "check" ? "Checking…" : "Check again"}
            </Button>
          </div>
        )}
        {outcomeFor("user")}
      </SettingsSection>
      {available && (
        <SettingsSection
          title="All users of this Mac"
          description="Installs for every account on this Mac. macOS asks for an administrator password."
        >
          <SystemRow
            link={available.system}
            status={available}
            work={work}
            busy={busy}
            onInstall={() => void change("install", "system")}
            onUninstall={() => void change("uninstall", "system")}
          />
          {outcomeFor("system")}
        </SettingsSection>
      )}
    </SettingsPanel>
  );
}

/**
 * The line an action leaves beside itself. A failure is an error; anything
 * else, a cancelled administrator prompt included, is a quiet note.
 */
function OutcomeLine({ outcome }: { outcome: Outcome }) {
  if (outcome.failed) return <SettingsError>{outcome.text}</SettingsError>;
  return (
    <p role="status" className="text-sm text-muted-foreground">
      {outcome.text}
    </p>
  );
}

function UserActions({
  link,
  work,
  busy,
  onInstall,
  onUninstall,
}: {
  link: CliLink;
  work: Work | null;
  busy: boolean;
  onInstall: () => void;
  onUninstall: () => void;
}) {
  switch (link.state) {
    case "foreign":
      return null;
    case "installed":
      return (
        <Button
          variant="outline"
          size="sm"
          disabled={busy}
          onClick={onUninstall}
        >
          {work === "uninstall:user"
            ? "Uninstalling…"
            : "Uninstall the command"}
        </Button>
      );
    case "stale":
      return (
        <>
          <Button size="sm" disabled={busy} onClick={onInstall}>
            {work === "install:user" ? "Repairing…" : "Repair the command"}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={onUninstall}
          >
            {work === "uninstall:user"
              ? "Uninstalling…"
              : "Uninstall the command"}
          </Button>
        </>
      );
    case "missing":
      return (
        <Button size="sm" disabled={busy} onClick={onInstall}>
          {work === "install:user"
            ? "Installing…"
            : "Install the tidebreak command"}
        </Button>
      );
  }
}

function SystemRow({
  link,
  status,
  work,
  busy,
  onInstall,
  onUninstall,
}: {
  link: CliLink;
  status: Available;
  work: Work | null;
  busy: boolean;
  onInstall: () => void;
  onUninstall: () => void;
}) {
  const path = <Path>{link.path}</Path>;
  const state =
    link.state === "installed" ? (
      <>Installed at {path}.</>
    ) : link.state === "stale" ? (
      <>
        {path} opens <Path>{homeRelative(link.target, status)}</Path>, which is
        not this app.
      </>
    ) : link.state === "foreign" ? (
      <>Tidebreak did not make {path}, so it will not replace it.</>
    ) : (
      <>Not installed. Installing links {path} to this app.</>
    );
  // Laid out like the section above: what is there, then what to do about
  // it, then what the last action said.
  const install =
    link.state === "missing" || link.state === "stale" ? (
      <Button variant="outline" size="sm" disabled={busy} onClick={onInstall}>
        {work === "install:system"
          ? "Waiting for macOS…"
          : link.state === "stale"
            ? "Repair for all users…"
            : "Install for all users…"}
      </Button>
    ) : null;
  const uninstall =
    link.state === "installed" || link.state === "stale" ? (
      <Button variant="outline" size="sm" disabled={busy} onClick={onUninstall}>
        {work === "uninstall:system"
          ? "Waiting for macOS…"
          : "Uninstall for all users…"}
      </Button>
    ) : null;
  return (
    <>
      <p className="text-sm text-muted-foreground">{state}</p>
      {(install || uninstall) && (
        <div className="flex flex-wrap gap-2">
          {install}
          {uninstall}
        </div>
      )}
    </>
  );
}
