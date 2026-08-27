// @vitest-environment jsdom
import {
  act,
  cleanup,
  createEvent,
  fireEvent,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ReactNode } from "react";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { HarnessDoctorEntry } from "../api/types";
import { renderWithRouter } from "@/test/router";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
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
  useCodeUiStore.setState({ lastCreate: null });
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
    installable: true,
    authenticated: true,
    tier: "reference",
    caps: { ...CAPS, structured_approvals: "supported", ...caps },
    commands: [],
    auth_mode: "local_sign_in",
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
  it("does not offer attachments before a real session exists", async () => {
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[entry("claude_code", {})]}
          starting={false}
          selectedMode={null}
          onSelectMode={vi.fn()}
          onStart={vi.fn()}
        />,
      ),
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    const paste = createEvent.paste(box, {
      clipboardData: {
        files: [
          new File([new Uint8Array([1, 2, 3])], "shot.png", {
            type: "image/png",
          }),
        ],
      },
    });
    fireEvent(box, paste);
    expect(paste.defaultPrevented).toBe(false);
    expect(screen.queryByLabelText("Attached images")).toBeNull();
    expect(document.querySelector('input[type="file"][accept]')).toBeNull();
  });

  it("defaults to the widest mode the engine honors and starts on Cmd+Enter", async () => {
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
    expect(screen.queryByText(/runs without asking/)).toBeNull();
    expect(
      screen.getByRole("combobox", { name: "Harness" }).closest("form"),
    ).toHaveClass("chat-composer");
    const field = screen.getByRole("textbox", { name: "Message" });
    await user.type(field, "list the files");
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    expect(onStart).toHaveBeenCalledWith(
      "claude_code",
      "allow",
      "list the files",
      undefined,
      undefined,
      null,
      false,
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
      undefined,
      null,
      false,
    );
  });

  it("switches an Auto-only engine to Auto without extra permission copy", async () => {
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
    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    await user.click(screen.getByRole("option", { name: /Grok CLI/ }));
    // The selected Ask is not honorable here; the mode follows the engine.
    expect(
      screen.getByRole("button", { name: "Permissions: Auto" }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/runs without asking/)).toBeNull();
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
      undefined,
      null,
      false,
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
      undefined,
      null,
      false,
    );
  });

  it("posts Grok's gateway-qualified model id", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    const efforts: ReasoningEffort[] = ["low", "medium", "high", "xhigh"];
    const client = {
      listCodeHarnessModels: vi.fn(async () => ({
        kind: "grok" as const,
        models: [
          {
            id: "model-gateway-model-gateway/grok-4.6",
            label: "Grok 4.6",
            default: true,
            reasoning_efforts: efforts,
            fast_mode: false,
          },
        ],
        reasoning_efforts: efforts,
      })),
    };
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[
            entry("grok", {
              plan_mode: "unsupported",
              structured_approvals: "unsupported",
              auto_mode: "supported",
              allow_mode: "supported",
            }),
          ]}
          starting={false}
          selectedMode={null}
          onSelectMode={vi.fn()}
          onStart={onStart}
          client={client}
          catalogModels={[
            {
              key: "model_gateway::grok-4.6",
              id: "grok-4.6",
              display_name: "Grok 4.6",
              provider: "model_gateway",
              vendor: "xai",
              available: true,
            } as never,
          ]}
          defaultModelKey="model_gateway::grok-4.6"
        />,
      ),
    );

    expect(client.listCodeHarnessModels).toHaveBeenCalledWith("grok");
    expect(
      await screen.findByRole("button", { name: "Model: Grok 4.6" }),
    ).toBeInTheDocument();
    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "list the files",
    );
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(onStart).toHaveBeenCalledWith(
      "grok",
      "allow",
      "list the files",
      "model-gateway-model-gateway/grok-4.6",
      undefined,
      null,
      false,
    );
  });

  it("keeps each harness model separate across switches", async () => {
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
                {
                  id: "opus",
                  label: "Opus",
                  default: false,
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
    await user.click(screen.getByRole("button", { name: "Model: Sonnet" }));
    await user.click(screen.getByRole("menuitem", { name: /Opus/ }));
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
          {
            id: "gpt-5.6-sol",
            label: "GPT 5.6 Sol",
            default: false,
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
    await user.click(
      screen.getByRole("button", { name: "Model: GPT 5.6 Luna" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /GPT 5.6 Sol/ }));

    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    await user.click(screen.getByRole("option", { name: /Claude Code/ }));
    expect(
      await screen.findByRole("button", { name: "Model: Opus" }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    await user.click(screen.getByRole("option", { name: /Codex CLI/ }));
    expect(
      await screen.findByRole("button", { name: "Model: GPT 5.6 Sol" }),
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

  it("offers reasoning effort and fast mode only when the engine and model honor them", async () => {
    const user = userEvent.setup();
    const efforts: ReasoningEffort[] = ["low", "medium", "high"];
    const client = {
      listCodeHarnessModels: vi.fn((kind: HarnessDoctorEntry["kind"]) =>
        Promise.resolve(
          kind === "opencode"
            ? {
                kind: "opencode" as const,
                models: [
                  {
                    id: "gpt-5.6-sol",
                    label: "GPT 5.6 Sol",
                    default: true,
                    reasoning_efforts: [],
                    fast_mode: false,
                  },
                ],
                reasoning_efforts: [],
                fast_mode: false,
              }
            : {
                kind: "claude_code" as const,
                models: [
                  {
                    id: "claude-opus-5",
                    label: "Claude Opus 5",
                    default: true,
                    reasoning_efforts: [],
                    fast_mode: true,
                  },
                  {
                    id: "claude-sonnet-5",
                    label: "Claude Sonnet 5",
                    default: false,
                    reasoning_efforts: [],
                    fast_mode: false,
                  },
                ],
                reasoning_efforts: efforts,
                fast_mode: true,
              },
        ),
      ),
    };
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[entry("claude_code", {}), entry("opencode", {})]}
          starting={false}
          selectedMode={null}
          onSelectMode={vi.fn()}
          onStart={vi.fn()}
          client={client}
        />,
      ),
    );

    expect(
      await screen.findByRole("button", { name: "Model: Claude Opus 5" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Reasoning: Default" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("switch", { name: "Fast mode off" }),
    ).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Model: Claude Opus 5" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /Claude Sonnet 5/ }));
    expect(
      screen.queryByRole("switch", { name: /Fast mode/ }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    await user.click(screen.getByRole("option", { name: /opencode/ }));
    expect(
      await screen.findByRole("button", { name: "Model: GPT 5.6 Sol" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Reasoning:/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: /Fast mode/ }),
    ).not.toBeInTheDocument();
  });

  it("posts the chosen reasoning effort and fast mode on start", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    const efforts: ReasoningEffort[] = ["low", "medium", "high"];
    const client = {
      listCodeHarnessModels: vi.fn(async () => ({
        kind: "claude_code" as const,
        models: [
          {
            id: "claude-opus-5",
            label: "Claude Opus 5",
            default: true,
            reasoning_efforts: [],
            fast_mode: true,
          },
        ],
        reasoning_efforts: efforts,
        fast_mode: true,
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
    await user.click(
      screen.getByRole("button", { name: "Reasoning: Default" }),
    );
    await user.click(screen.getByRole("menuitem", { name: "High" }));
    await user.click(screen.getByRole("switch", { name: "Fast mode off" }));
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
      undefined,
      "high",
      true,
    );
  });

  it("restores lastCreate effort and fast mode only when the engine still honors them", async () => {
    useCodeUiStore.setState({
      lastCreate: {
        harness: "opencode",
        modelsByHarness: { opencode: "gpt-5.6-sol" },
        reasoningEffortByHarness: { opencode: "high" },
        fastModeByHarness: { opencode: true },
      },
    });
    const client = {
      listCodeHarnessModels: vi.fn(async () => ({
        kind: "opencode" as const,
        models: [
          {
            id: "gpt-5.6-sol",
            label: "GPT 5.6 Sol",
            default: true,
            reasoning_efforts: [],
            fast_mode: false,
          },
        ],
        reasoning_efforts: [],
        fast_mode: false,
      })),
    };
    const onStart = vi.fn();
    const user = userEvent.setup();
    await renderWithRouter(
      wrap(
        <StartSessionPrompt
          workspaceId="workspace-1"
          harnesses={[entry("opencode", {})]}
          starting={false}
          selectedMode={null}
          onSelectMode={vi.fn()}
          onStart={onStart}
          client={client}
        />,
      ),
    );
    expect(
      await screen.findByRole("button", { name: "Model: GPT 5.6 Sol" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Reasoning:/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: /Fast mode/ }),
    ).not.toBeInTheDocument();
    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "list the files",
    );
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(onStart).toHaveBeenCalledWith(
      "opencode",
      "allow",
      "list the files",
      "gpt-5.6-sol",
      undefined,
      null,
      false,
    );
    await waitFor(() => expect(onStart).toHaveBeenCalled());
  });
});
