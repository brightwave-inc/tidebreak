import type {
  CliCommandChange,
  CliCommandHost,
  CliCommandStatus,
  CliLink,
  CliLocation,
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

function link(path: string, state: Partial<CliLink> = {}): CliLink {
  return { path, state: "missing", onPath: true, ...state } as CliLink;
}

function available(
  user: Partial<CliLink> = {},
  system: Partial<CliLink> = {},
  resolved: string | null = null,
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
  installed: available({ state: "installed" }, {}, userPath),
  notOnPath: available({ state: "installed", onPath: false }),
  foreign: available(
    { state: "foreign", target: `${CLI_HOME}/.cargo/bin/tidebreak` },
    {},
    `${CLI_HOME}/.cargo/bin/tidebreak`,
  ),
  stale: available({
    state: "stale",
    target: `${CLI_HOME}/Downloads/Tidebreak.app/Contents/MacOS/tidebreak`,
  }),
  systemInstalled: available(
    { state: "missing" },
    { state: "installed" },
    systemPath,
  ),
  temporaryLocation: { status: "unavailable", reason: "temporary_location" },
  notBundled: { status: "unavailable", reason: "not_bundled" },
} satisfies Record<string, CliCommandStatus>;

/**
 * A host that serves `status` and answers each install or uninstall with
 * `after`, or with `failure` when one is given.
 */
export function cliCommandFixtureHost({
  status,
  after = status,
  failure,
  native = true,
}: {
  status: CliCommandStatus;
  after?: CliCommandStatus;
  failure?: string;
  native?: boolean;
}): CliCommandHost {
  let current = status;
  const answer = async (
    location: CliLocation,
    outcome: CliCommandChange["outcome"],
  ): Promise<CliCommandChange> => {
    if (failure) throw failure;
    current = after;
    return { location, outcome, folderCreated: false, status: current };
  };
  return {
    available: () => native,
    status: async () => current,
    install: (location) => answer(location, "created"),
    uninstall: (location) => answer(location, "removed"),
  };
}
