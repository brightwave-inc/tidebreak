// @vitest-environment jsdom
import { act, cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ReactNode } from "react";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { HarnessDoctorEntry } from "../api/types";
import { renderWithRouter } from "@/test/router";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { ALLOW_ALL_NOTE, UNSUPERVISED_AUTO_NOTE } from "./labels";
import { StartSessionPrompt } from "./StartSessionPrompt";
import type { ReasoningEffort } from "../api/types";
import type { ParsedHarnessModel } from "./parsers";

function app(): AppContextValue {
  return {
    client: {} as never,
    models: [],
    defaultModelKey: null,
    providers: [],
    refreshCatalog: async () => {},
    refreshChats: async () => {},
    status: "",
    setStatus: () => {},
    newChat: () => {},
    deleteChat: () => {},
    startRename: () => {},
    commitRename: () => {},
    cancelRename: () => {},
    newProject: async () => false,
    deleteProject: () => {},
    startProjectRename: () => {},
    commitProjectRename: () => {},
    cancelProjectRename: () => {},
    newChatInProject: () => {},
    moveChatToProject: () => {},
    updateState: { status: "idle", version: null, error: null, enabled: false },
    updateUpToDate: false,
    checkForUpdate: async () => ({
      status: "idle",
      version: null,
      error: null,
      enabled: false,
    }),
    attachment: "local",
    restartForUpdate: async () => {},
  };
}

function wrap(ui: ReactNode) {
  return <AppContextProvider value={app()}>{ui}</AppContextProvider>;
}

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
});

const CAPS = {
  resume: "supported",
  streaming_deltas: "supported",
  mid_turn_steering: "unsupported",
  plan_mode: "supported",
  auto_mode: "supported",
  allow_mode: "supported",
  reasoning_levels: "unknown",
  native_file_change_events: "unsupported",
  native_interrupt: "supported",
  image_input: "unknown",
  slash_commands: "unknown",
} as const;

function entry(
  kind: HarnessDoctorEntry["kind"],
  caps: Partial<HarnessDoctorEntry["caps"]>,
): HarnessDoctorEntry {
  return {
    kind,
    found: true,
    tier: "reference",
    caps: { ...CAPS, structured_approvals: "supported", ...caps },
    commands: [],
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("StartSessionPrompt", () => {
  it("defaults to the widest mode the engine honors, says so, and starts on Cmd+Enter", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[entry("claude_code", {})]}
          starting={false}
          selectedMode={null}
          onSelectMode={vi.fn()}
          onStart={onStart}
        />,
      ),
    );
    expect(
      screen.getByRole("button", { name: "Permissions: Allow all" }),
    ).toBeInTheDocument();
    expect(screen.getByText(ALLOW_ALL_NOTE)).toBeInTheDocument();
    const field = screen.getByRole("textbox", { name: "Message" });
    await user.type(field, "list the files");
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    expect(onStart).toHaveBeenCalledWith(
      "claude_code",
      "allow",
      "list the files",
      undefined,
    );
  });

  it("falls back to Plan when structured approvals are not supported", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[
            entry("claude_code", {
              structured_approvals: "unsupported",
              auto_mode: "unsupported",
              allow_mode: "unsupported",
            }),
          ]}
          starting={false}
          selectedMode={null}
          onSelectMode={vi.fn()}
          onStart={onStart}
        />,
      ),
    );
    expect(
      screen.getByRole("button", { name: "Permissions: Plan" }),
    ).toBeInTheDocument();
    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "list the files",
    );
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(onStart).toHaveBeenCalledWith(
      "claude_code",
      "plan",
      "list the files",
      undefined,
    );
  });

  it("switches an Auto-only engine to unsupervised Auto and says so", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[
            entry("claude_code", {}),
            entry("grok", {
              plan_mode: "unsupported",
              structured_approvals: "unsupported",
              allow_mode: "unsupported",
            }),
          ]}
          starting={false}
          selectedMode="ask"
          onSelectMode={vi.fn()}
          onStart={onStart}
        />,
      ),
    );
    expect(screen.queryByText(UNSUPERVISED_AUTO_NOTE)).toBeNull();
    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    await user.click(screen.getByRole("option", { name: /Grok CLI/ }));
    // The selected Ask is not honorable here; the mode follows the engine.
    expect(
      screen.getByRole("button", { name: "Permissions: Auto" }),
    ).toBeInTheDocument();
    expect(screen.getByText(UNSUPERVISED_AUTO_NOTE)).toBeInTheDocument();
    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "list the files",
    );
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(onStart).toHaveBeenCalledWith(
      "grok",
      "auto",
      "list the files",
      undefined,
    );
  });

  it("posts the listed default model when starting", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    const client = {
      listCodeHarnessModels: vi.fn(async () => ({
        kind: "claude_code" as const,
        models: [
          {
            id: "claude-opus-5",
            label: "Claude Opus 5",
            default: true,
            reasoning_efforts: [],
            fast_mode: false,
          },
        ],
        reasoning_efforts: [],
        fast_mode: false,
      })),
    };
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[entry("claude_code", {})]}
          starting={false}
          selectedMode={null}
          onSelectMode={vi.fn()}
          onStart={onStart}
          client={client}
        />,
      ),
    );
    expect(
      await screen.findByRole("button", { name: "Model: Claude Opus 5" }),
    ).toBeInTheDocument();
    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "list the files",
    );
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(onStart).toHaveBeenCalledWith(
      "claude_code",
      "allow",
      "list the files",
      "claude-opus-5",
    );
  });

  it("never carries a model across a harness switch", async () => {
    const user = userEvent.setup();
    const codex = deferred<{
      kind: "codex";
      models: ParsedHarnessModel[];
      reasoning_efforts: ReasoningEffort[];
    }>();
    const client = {
      listCodeHarnessModels: vi.fn((kind: HarnessDoctorEntry["kind"]) =>
        kind === "codex"
          ? codex.promise
          : Promise.resolve({
              kind: "claude_code" as const,
              models: [
                {
                  id: "sonnet",
                  label: "Sonnet",
                  default: true,
                  reasoning_efforts: [],
                  fast_mode: false,
                },
              ],
              reasoning_efforts: [],
              fast_mode: false,
            }),
      ),
    };
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[entry("claude_code", {}), entry("codex", {})]}
          starting={false}
          selectedMode={null}
          onSelectMode={vi.fn()}
          onStart={vi.fn()}
          client={client}
        />,
      ),
    );

    expect(
      await screen.findByRole("button", { name: "Model: Sonnet" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    await user.click(screen.getByRole("option", { name: /Codex CLI/ }));

    expect(
      screen.queryByRole("button", { name: "Model: Sonnet" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Loading models" }),
    ).toBeDisabled();

    await act(async () => {
      codex.resolve({
        kind: "codex",
        models: [
          {
            id: "gpt-5.6-luna",
            label: "GPT 5.6 Luna",
            default: true,
            reasoning_efforts: [],
            fast_mode: false,
          },
        ],
        reasoning_efforts: [],
      });
    });
    expect(
      await screen.findByRole("button", { name: "Model: GPT 5.6 Luna" }),
    ).toBeInTheDocument();
  });

  it("narrows to a picked mode", async () => {
    const user = userEvent.setup();
    const onSelectMode = vi.fn();
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[entry("claude_code", {})]}
          starting={false}
          selectedMode="allow"
          onSelectMode={onSelectMode}
          onStart={vi.fn()}
        />,
      ),
    );
    await user.click(
      screen.getByRole("button", { name: "Permissions: Allow all" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /Ask/ }));
    expect(onSelectMode).toHaveBeenCalledWith("ask");
  });
});
