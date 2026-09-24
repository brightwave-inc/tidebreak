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
 * A path with the home folder written as `~`, the way a terminal shows it.
 * The home folder is read off the account's own link, which always sits at
 * `<home>/.local/bin/tidebreak`.
 */
function homeRelative(path: string, status: Available): string {
  const home = status.user.path.replace(/\/\.local\/bin\/tidebreak$/, "");
  return home && home !== status.user.path && path.startsWith(`${home}/`)
    ? `~${path.slice(home.length)}`
    : path;
}

function Path({ children }: { children: string }) {
  return <code className="break-all font-mono text-xs">{children}</code>;
}

type Verdict = {
  tone: SettingsStatusTone;
  label: string;
  description: ReactNode;
};

/** The verdict the page leads with. */
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
  switch (user.state) {
    case "foreign":
      return {
        tone: "warning",
        label: "Another tidebreak is in the way",
        description: (
          <>
            Tidebreak did not make {userPath}, so it will not replace it. Move
            or delete it, then install the command.
          </>
        ),
      };
    case "stale":
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
    case "installed":
      if (user.onPath === false) {
        return {
          tone: "warning",
          label: "Installed, but not on your PATH",
          description: (
            <>
              The command is at {userPath}, but a new terminal does not look in
              that folder. Add this line to your shell profile, such as{" "}
              <Path>~/.zshrc</Path>, then open a new terminal:{" "}
              <Path>{LOCAL_BIN_PATH_LINE}</Path>
            </>
          ),
        };
      }
      if (resolved && resolved !== user.path) {
        return {
          tone: "warning",
          label: "Another tidebreak runs first",
          description: (
            <>
              A new terminal runs <Path>{homeRelative(resolved, status)}</Path>{" "}
              before {userPath}. Remove it, or put <Path>~/.local/bin</Path>{" "}
              earlier in your PATH.
            </>
          ),
        };
      }
      return {
        tone: "ready",
        label: "Installed",
        description: (
          <>
            Run <Path>tidebreak</Path> in a new terminal. It links to this app,
            so updates keep it current.
          </>
        ),
      };
    case "missing":
      if (system.state === "installed") {
        return {
          tone: "ready",
          label: "Installed for all users",
          description: (
            <>
              Run <Path>tidebreak</Path> in a new terminal. It links to this app
              from <Path>{system.path}</Path>.
            </>
          ),
        };
      }
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
}

/** What an install or uninstall did, in one sentence. */
export function changeSummary(change: CliCommandChange): string | null {
  if (change.status.status !== "available") return null;
  const link =
    change.location === "user" ? change.status.user : change.status.system;
  const path = homeRelative(link.path, change.status);
  switch (change.outcome) {
    case "created":
      return change.folderCreated
        ? `Created ${path.replace(/\/tidebreak$/, "")} and linked the command there.`
        : `Linked ${path} to this app.`;
    case "updated":
      return `Pointed ${path} at this app.`;
    case "unchanged":
      return `${path} already links to this app.`;
    case "removed":
      return `Removed ${path}.`;
    case "absent":
      return `${path} was already gone.`;
  }
}

type Work = `${"install" | "uninstall"}:${CliLocation}` | "check";

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
 * The app menu's Install the tidebreak Command lands here with `autoInstall`
 * set, so the install runs once and its result is on the page that explains
 * it.
 */
export function CommandLinePanel({
  host = cliCommandHost,
  autoInstall = false,
  onAutoInstallHandled,
}: {
  host?: CliCommandHost;
  autoInstall?: boolean;
  onAutoInstallHandled?: () => void;
}) {
  const native = host.available();
  const [status, setStatus] = useState<CliCommandStatus | null>(null);
  const [work, setWork] = useState<Work | null>(native ? "check" : null);
  const [summary, setSummary] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const check = useCallback(async () => {
    if (!native) return;
    setWork("check");
    try {
      const next = await host.status();
      if (!mounted.current) return;
      setStatus(next);
      setError(null);
    } catch (err) {
      if (mounted.current) setError(String(err));
    } finally {
      if (mounted.current) setWork(null);
    }
  }, [host, native]);

  const change = useCallback(
    async (action: "install" | "uninstall", location: CliLocation) => {
      setWork(`${action}:${location}`);
      setSummary(null);
      setError(null);
      try {
        const result = await host[action](location);
        if (!mounted.current) return;
        setStatus(result.status);
        setSummary(changeSummary(result));
      } catch (err) {
        if (!mounted.current) return;
        setError(String(err));
        // A refusal says why; the state behind it is worth reading afresh.
        void host
          .status()
          .then((next) => mounted.current && setStatus(next))
          .catch(() => undefined);
      } finally {
        if (mounted.current) setWork(null);
      }
    },
    [host],
  );

  // One pass on mount: run the install the menu asked for, or read the
  // status. The first props decide, and the ref keeps a second run of the
  // effect from installing twice.
  const firstPass = useRef({ autoInstall, onAutoInstallHandled, ran: false });
  useEffect(() => {
    const pass = firstPass.current;
    if (pass.ran) return;
    pass.ran = true;
    if (pass.autoInstall && native) {
      pass.onAutoInstallHandled?.();
      void change("install", "user");
    } else {
      void check();
    }
  }, [change, check, native]);

  const verdict = commandLineVerdict(status, native);
  const available = status?.status === "available" ? status : null;
  const busy = work !== null;

  return (
    <SettingsPanel
      title="Command line"
      description="Run Tidebreak from a terminal with the tidebreak command that ships inside the app."
      busy={busy}
    >
      <SettingsSection
        title="The tidebreak command"
        description="Installs for your account in ~/.local/bin, without a password."
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
        {summary && (
          <p role="status" className="text-sm text-muted-foreground">
            {summary}
          </p>
        )}
        {error && <SettingsError>{error}</SettingsError>}
        {available && (
          <div className="flex flex-wrap gap-2">
            <UserActions
              link={available.user}
              work={work}
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
      </SettingsSection>
      {available && (
        <SettingsSection
          title="All users of this Mac"
          description="Links the command in /usr/local/bin. macOS asks for an administrator password."
        >
          <SystemRow
            link={available.system}
            status={available}
            work={work}
            onInstall={() => void change("install", "system")}
            onUninstall={() => void change("uninstall", "system")}
          />
        </SettingsSection>
      )}
    </SettingsPanel>
  );
}

function UserActions({
  link,
  work,
  onInstall,
  onUninstall,
}: {
  link: CliLink;
  work: Work | null;
  onInstall: () => void;
  onUninstall: () => void;
}) {
  const busy = work !== null;
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
  onInstall,
  onUninstall,
}: {
  link: CliLink;
  status: Available;
  work: Work | null;
  onInstall: () => void;
  onUninstall: () => void;
}) {
  const busy = work !== null;
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
      <>Not installed for all users.</>
    );
  return (
    <div className="flex flex-wrap items-center justify-between gap-3">
      <p className="min-w-0 flex-1 basis-56 text-sm text-muted-foreground">
        {state}
      </p>
      {link.state === "missing" || link.state === "stale" ? (
        <Button variant="outline" size="sm" disabled={busy} onClick={onInstall}>
          {work === "install:system"
            ? "Waiting for macOS…"
            : link.state === "stale"
              ? "Repair for all users…"
              : "Install for all users…"}
        </Button>
      ) : link.state === "installed" ? (
        <Button
          variant="outline"
          size="sm"
          disabled={busy}
          onClick={onUninstall}
        >
          {work === "uninstall:system"
            ? "Waiting for macOS…"
            : "Uninstall for all users…"}
        </Button>
      ) : null}
    </div>
  );
}
