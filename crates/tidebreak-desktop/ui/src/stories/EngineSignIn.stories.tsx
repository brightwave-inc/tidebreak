import { useMemo } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import type { HarnessDoctorEntry, HarnessKind } from "@/api/types";
import {
  EngineSignInDialog,
  type EngineSignInClient,
  HarnessSignInNote,
} from "@/code/EngineSignIn";
import { encodeTerminalBytes } from "@/code/TerminalPane";
import { harnessDoctor } from "./fixtures";

/**
 * Engine sign-in: the engine's own login command, run in a terminal inside
 * Tidebreak, and what the check found when it exits.
 *
 * Tidebreak runs the engine binaries it downloaded, which are not on the
 * reader's `PATH`, so this dialog is where a downloaded engine gets signed
 * in. The doctor rows, the new-workspace picker, and a failed Codex turn all
 * open it.
 */

type Scenario =
  | "waiting"
  | "signed-in"
  | "signed-out"
  | "unconfirmed"
  | "checking"
  | "cannot-start";

/**
 * What each engine's own sign-in prints while it waits, and then as it
 * finishes. Illustrative, not a capture: identifiers are placeholders.
 */
const OUTPUT: Partial<Record<HarnessKind, { waiting: string; done: string }>> =
  {
    claude_code: {
      waiting:
        "Opening your browser to sign in to Claude Code…\r\n\r\n" +
        "If the browser did not open, visit:\r\n" +
        "https://claude.ai/oauth/authorize?code=true&client_id=CLIENT_ID&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&state=STATE\r\n\r\n" +
        "Paste the code here if prompted > ",
      done: "\r\nLogin successful.\r\n",
    },
    codex: {
      waiting:
        "Starting local login server on http://localhost:1455.\r\n" +
        "If your browser did not open, navigate to this URL to authenticate:\r\n\r\n" +
        "https://auth.openai.com/oauth/authorize?response_type=code&client_id=CLIENT_ID&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=STATE\r\n\r\n" +
        "On a remote or headless machine? Use `codex login --device-auth` instead.\r\n",
      done: "Successfully logged in\r\n",
    },
    opencode: {
      waiting:
        "┌  Add credential\r\n│\r\n" +
        "◆  Select provider\r\n" +
        "│  ● Anthropic (recommended)\r\n" +
        "│  ○ OpenAI\r\n" +
        "│  ○ Google\r\n" +
        "│  ○ OpenRouter\r\n└\r\n",
      done: "└  Done\r\n",
    },
  };

function entryFor(
  kind: HarnessKind,
  authenticated: boolean | undefined,
): HarnessDoctorEntry {
  const base =
    harnessDoctor.harnesses.find((entry) => entry.kind === kind) ??
    harnessDoctor.harnesses[0];
  return { ...base, kind, authenticated };
}

function storyClient(
  kind: HarnessKind,
  scenario: Scenario,
): EngineSignInClient {
  const output = OUTPUT[kind] ?? { waiting: "", done: "" };
  const finished = scenario !== "waiting" && scenario !== "cannot-start";
  const ending =
    scenario === "signed-out" ? "\r\nSign-in cancelled.\r\n" : output.done;
  const text = finished ? output.waiting + ending : output.waiting;
  const terminal = {
    id: `sign-in-${kind}`,
    kind,
    command: entryFor(kind, false).sign_in_command ?? "",
    cols: 100,
    rows: 20,
    ended: false,
    created_at: "2026-09-23T12:00:00.000Z",
  };
  return {
    startHarnessSignIn: async () => {
      if (scenario === "cannot-start") {
        throw new Error("Download Claude Code before you sign in to it");
      }
      return terminal;
    },
    readHarnessSignIn: async (_kind, _id, cursor = 0) => ({
      id: terminal.id,
      kind,
      bytes: cursor === 0 ? encodeTerminalBytes(text) : "",
      cursor: text.length,
      overflow: false,
      truncated: false,
      ended: finished,
    }),
    writeHarnessSignIn: async () => {},
    resizeHarnessSignIn: async () => terminal,
    closeHarnessSignIn: async () => {},
  };
}

function SignInStory({
  kind,
  scenario,
}: {
  kind: HarnessKind;
  scenario: Scenario;
}) {
  const client = useMemo(() => storyClient(kind, scenario), [kind, scenario]);
  const authenticated =
    scenario === "signed-in"
      ? true
      : scenario === "unconfirmed"
        ? undefined
        : false;
  return (
    <EngineSignInDialog
      client={client}
      kind={kind}
      entry={entryFor(kind, authenticated)}
      open
      onOpenChange={fn()}
      onRecheck={
        scenario === "checking"
          ? () => new Promise<void>(() => {})
          : async () => {}
      }
    />
  );
}

const meta = {
  title: "Code/Engine sign-in",
  component: SignInStory,
  parameters: { layout: "fullscreen" },
  args: { kind: "claude_code", scenario: "waiting" },
} satisfies Meta<typeof SignInStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * The sign-in is running and waiting on the browser. Cancel stops it; the
 * line under the terminal says the check happens by itself.
 */
export const WaitingForTheBrowser: Story = {};

/** Codex prints its own URL and a device-code hint for a remote machine. */
export const CodexWaiting: Story = {
  args: { kind: "codex" },
};

/** opencode asks which provider to sign in to, in the terminal. */
export const OpencodeChoosesAProvider: Story = {
  args: { kind: "opencode" },
};

/** The command exited and the check is still reading the engine's state. */
export const Checking: Story = {
  args: { scenario: "checking" },
};

/** The check found the engine signed in. Done is the only way on. */
export const SignedIn: Story = {
  args: { scenario: "signed-in" },
};

/** The command exited without a sign-in. Run it again, or leave it for later. */
export const StillSignedOut: Story = {
  args: { scenario: "signed-out" },
};

/**
 * The check could not tell. opencode with only a local model lands here,
 * and the engine stays pickable.
 */
export const NotConfirmed: Story = {
  args: { kind: "opencode", scenario: "unconfirmed" },
};

/** The server refused to start the command; the terminal says why. */
export const CouldNotStart: Story = {
  args: { scenario: "cannot-start" },
};

/**
 * The line under the new-workspace engine pills: the engine downloaded but
 * still needs a sign-in, and the button that runs it.
 */
export const PickerNote: Story = {
  render: () => (
    <div className="bg-background mx-auto flex max-w-xl flex-col gap-3 p-8">
      <HarnessSignInNote entry={entryFor("claude_code", false)} />
      <HarnessSignInNote entry={entryFor("opencode", undefined)} />
    </div>
  ),
};
