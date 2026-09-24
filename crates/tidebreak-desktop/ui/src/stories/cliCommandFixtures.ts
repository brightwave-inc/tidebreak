import type {
  CliCommandChange,
  CliCommandHost,
  CliCommandStatus,
  CliLink,
  CliLocation,
  CliResolvedCommand,
} from "@/cliCommand";

/**
 * Typed fixtures for Settings → Command line, shared by its tests and
 * stories so both show the same states.
 */
export const CLI_HOME = "/Users/avery";
export const CLI_COMMAND =
  "/Applications/Tidebreak.app/Contents/MacOS/tidebreak";

const userPath = `${CLI_HOME}/.local/bin/tidebreak`;
const systemPath = "/usr/local/bin/tidebreak";
const cargoBuild = `${CLI_HOME}/.cargo/bin/tidebreak`;
const downloadsCopy = `${CLI_HOME}/Downloads/Tidebreak.app/Contents/MacOS/tidebreak`;

function link(path: string, state: Partial<CliLink> = {}): CliLink {
  return { path, state: "missing", onPath: true, ...state } as CliLink;
}

/** What a new terminal runs, and whether that is this app. */
function runs(path: string, thisApp: boolean): CliResolvedCommand {
  return { path, thisApp };
}

function available(
  user: Partial<CliLink> = {},
  system: Partial<CliLink> = {},
  resolved: CliResolvedCommand | null = null,
): CliCommandStatus {
  return {
    status: "available",
    command: CLI_COMMAND,
    user: link(userPath, user),
    system: link(systemPath, system),
    resolved,
  };
}

export const CLI_STATUS = {
  notInstalled: available({ state: "missing" }),
  installed: available({ state: "installed" }, {}, runs(userPath, true)),
  notOnPath: available({ state: "installed", onPath: false }),
  /** Another tidebreak comes earlier on the PATH than this account's link. */
  userShadowed: available({ state: "installed" }, {}, runs(cargoBuild, false)),
  foreign: available(
    { state: "foreign", target: cargoBuild },
    {},
    runs(cargoBuild, false),
  ),
  stale: available({ state: "stale", target: downloadsCopy }),
  systemInstalled: available(
    { state: "missing" },
    { state: "installed" },
    runs(systemPath, true),
  ),
  /**
   * Both links are in place, and the PATH lists /usr/local/bin first, as
   * pipx's setup does. Both run this app.
   */
  bothInstalledSystemFirst: available(
    { state: "installed" },
    { state: "installed" },
    runs(systemPath, true),
  ),
  /** The link for all users is in place, but the PATH omits its folder. */
  systemNotOnPath: available(
    { state: "missing" },
    { state: "installed", onPath: false },
  ),
  /** The link for all users is in place, but another tidebreak runs first. */
  systemShadowed: available(
    { state: "missing" },
    { state: "installed" },
    runs(cargoBuild, false),
  ),
  /** The link for all users points at another copy of the app. */
  systemStale: available(
    { state: "installed" },
    { state: "stale", target: downloadsCopy },
    runs(userPath, true),
  ),
  temporaryLocation: { status: "unavailable", reason: "temporary_location" },
  notBundled: { status: "unavailable", reason: "not_bundled" },
} satisfies Record<string, CliCommandStatus>;

/**
 * A host that serves `status` and answers each install or uninstall with
 * `after`, or with `failure` when one is given. `outcome` overrides what the
 * change reports; a `cancelled` one leaves the status as it was, as a
 * cancelled administrator prompt does.
 */
export function cliCommandFixtureHost({
  status,
  after = status,
  failure,
  outcome,
  native = true,
}: {
  status: CliCommandStatus;
  after?: CliCommandStatus;
  failure?: string;
  outcome?: CliCommandChange["outcome"];
  native?: boolean;
}): CliCommandHost {
  let current = status;
  const answer = async (
    location: CliLocation,
    done: CliCommandChange["outcome"],
  ): Promise<CliCommandChange> => {
    if (failure) throw failure;
    if (outcome !== "cancelled") current = after;
    return {
      location,
      outcome: outcome ?? done,
      folderCreated: false,
      status: current,
    };
  };
  return {
    available: () => native,
    status: async () => current,
    install: (location) => answer(location, "created"),
    uninstall: (location) => answer(location, "removed"),
  };
}
