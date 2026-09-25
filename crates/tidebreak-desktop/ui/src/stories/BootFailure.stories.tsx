import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { HttpError } from "@/api";
import { BootFailure } from "@/BootFailure";
import type { BootFailureKind } from "@/bootRecovery";

const DATA_DIR =
  "/Users/alex/Library/Application Support/io.brightwave.tidebreak";

const LOCAL = {
  attachment: "local",
  baseUrl: null,
  gatewayAuth: false,
} as const;

/** A local server that did not start, the way the desktop reports it. */
function localFailure(kind: BootFailureKind, error: string) {
  return {
    stage: "connect" as const,
    attachment: LOCAL,
    error,
    local: { kind, stopped: false, dataDir: DATA_DIR },
  };
}

/**
 * The screen the shell falls back to when it cannot reach the API it is
 * attached to.
 *
 * Worth a story because it is otherwise only reachable by breaking the
 * network: the states below are the ones that decide whether a reader can get
 * themselves out of it, and they differ mostly in which recovery is on offer.
 */
const meta = {
  title: "Shell/Boot failure",
  component: BootFailure,
  parameters: { layout: "fullscreen" },
  args: {
    stage: "catalog",
    error: new TypeError("Load failed"),
    appVersion: "0.58.0",
    onRetry: fn(),
    onWorkLocally: fn(async () => {}),
    onRestart: fn(async () => {}),
    onRevealDataDir: fn(async () => {}),
    onReportProblem: fn(),
    writeClipboard: fn(async () => {}),
    attachment: {
      attachment: "remote",
      baseUrl: "https://tidebreak.example.com",
      gatewayAuth: true,
    },
  },
} satisfies Meta<typeof BootFailure>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * The state this screen exists for: the window is attached to another machine
 * that stopped answering, so the reader is offered a way back to this one.
 */
export const UnreachableRemoteMachine: Story = {};

/**
 * The embedded server failed to start. Nothing to detach from, so retry is the
 * only recovery — and the reader still gets the error and a way to report it.
 */
export const LocalServerFailed: Story = {
  args: {
    stage: "connect",
    error: new Error("another instance already owns this data directory"),
    attachment: { attachment: "local", baseUrl: null, gatewayAuth: false },
  },
};

/**
 * The connection resolved but the catalog did not. The machine is local, so
 * the headline names the step rather than an address.
 */
export const LocalCatalogFailed: Story = {
  args: {
    // The catalog read fails the way the HTTP client raises it.
    error: new HttpError(
      500,
      "500: could not read the model catalog",
      "internal",
      {
        kind: "internal",
        message: "could not read the model catalog",
      },
    ),
    attachment: { attachment: "local", baseUrl: null, gatewayAuth: false },
  },
};

/**
 * The shell could not even read its own attachment — the boot screen degrades
 * to the generic sentence rather than claiming a machine it cannot name.
 */
export const AttachmentUnknown: Story = {
  args: { stage: "connect", attachment: null },
};

/**
 * A long address and a long error, which is the realistic shape of a failure
 * worth copying. Pins that neither one breaks the layout.
 */
export const LongDetail: Story = {
  args: {
    attachment: {
      attachment: "remote",
      baseUrl:
        "https://tidebreak.some-quite-long-internal-hostname.example.com",
      gatewayAuth: true,
    },
    error: new Error(
      "TypeError: Load failed — the machine did not answer within the " +
        "request timeout, and no response headers were received",
    ),
  },
};

/** Hosted discovery failed before the browser could identify its attachment. */
export const HostedDiscoveryUnavailable: Story = {
  args: {
    stage: "connect",
    attachment: null,
    error: new Error(
      "Could not reach Tidebreak at https://tidebreak.example.com. Try again. " +
        "If the connection still fails, contact your administrator.",
    ),
  },
};

/**
 * Another process holds the data folder's lock, such as `tidebreak serve`.
 * Try again runs the boot again, so quitting that process is enough.
 */
export const LocalDataFolderInUse: Story = {
  args: localFailure(
    "instance_lock",
    `configuration error: another Tidebreak process is already running on the data directory ${DATA_DIR}. Quit that process and try again, or set TIDEBREAK_DATA_DIR to another folder.`,
  ),
};

/** A newer version wrote this profile, so this one sends you to it. */
export const LocalDataFromNewerVersion: Story = {
  args: localFailure(
    "newer_version",
    "This Tidebreak profile was written by a newer version. Install that version or later, or restore a backup.",
  ),
};

/** The schema marker is from another lifecycle, or damaged. */
export const LocalDataNotRecognized: Story = {
  args: localFailure(
    "unrecognized_data",
    'configuration error: refusing to open local SQLite database for schema marker lifecycle "v2", epoch 1',
  ),
};

/** The 1.0 product-major guard: a build that shipped without a lifecycle. */
export const LocalVersionCannotOpenData: Story = {
  args: localFailure(
    "unsupported_version",
    "configuration error: this build is major version 2, and no local profile lifecycle is defined for it",
  ),
};

/** Opening or updating the database failed; the host's words follow. */
export const LocalMigrationFailed: Story = {
  args: localFailure(
    "migration",
    "store error: could not open or update the local database: Migration Error: no such table: turn",
  ),
};

/** The disk has no room for the database to open. */
export const LocalDiskFull: Story = {
  args: localFailure(
    "disk_full",
    "store error: could not open or update the local database: database or disk is full",
  ),
};

/** The keychain refused, or is locked. */
export const LocalKeychainLocked: Story = {
  args: localFailure("keychain", "secret error: the keychain is locked"),
};

/** A failure with no sentence of its own keeps the host's words. */
export const LocalUnknownFailure: Story = {
  args: localFailure("unknown", "server error: the listener could not bind"),
};

/**
 * The server started, then its accept loop died. Nothing can start it again
 * inside this process, so the screen offers a restart.
 */
export const LocalServerStopped: Story = {
  args: {
    ...localFailure("unknown", "server error: connection reset"),
    local: { kind: "unknown", stopped: true, dataDir: DATA_DIR },
  },
};

/** A mapped failure at the minimum window. */
export const LocalDataFolderInUseMinimumWindow: Story = {
  ...LocalDataFolderInUse,
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};
