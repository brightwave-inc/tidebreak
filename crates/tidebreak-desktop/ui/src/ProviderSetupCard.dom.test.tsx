// @vitest-environment jsdom
import { useState } from "react";
import {
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ModelInfo } from "./api";
import { Composer, type ComposerSendBlocker } from "./Composer";
import {
  noModelSendBlocker,
  noRunnableModel,
  useRefreshWhileNoModelRuns,
} from "./modelSetup";
import { ProviderSetupCard } from "./ProviderSetupCard";
import { WelcomeState } from "./WelcomeState";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function model(available: boolean): ModelInfo {
  return {
    key: "anthropic::claude-opus-5",
    id: "claude-opus-5",
    display_name: "Claude Opus 5",
    provider: "anthropic",
    vendor: null,
    verification: "verified",
    recommended: true,
    available,
    context_window: 1_000_000,
    max_output_tokens: 128_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: [],
    multimodal: true,
  };
}

describe("when no model can run", () => {
  it("waits for the catalog before calling it empty", () => {
    expect(noRunnableModel([], false)).toBe(false);
    expect(noRunnableModel([], true)).toBe(true);
    expect(noRunnableModel([model(false)], undefined)).toBe(true);
    expect(noRunnableModel([model(false), model(true)], true)).toBe(false);
  });

  it("points a send at provider setup, or at the gateway when managed", () => {
    const navigate = vi.fn();
    const open = noModelSendBlocker(true, false, navigate as never);
    expect(open?.reason).toBe("Connect a model provider to send.");
    open?.action?.onClick();
    expect(navigate).toHaveBeenCalledWith({
      to: "/settings/providers",
      search: {},
    });

    const managed = noModelSendBlocker(true, true, navigate as never);
    expect(managed?.reason).toContain("organization's gateway");
    managed?.action?.onClick();
    expect(navigate).toHaveBeenLastCalledWith({ to: "/settings/gateway" });

    expect(noModelSendBlocker(false, false, navigate as never)).toBeNull();
  });
});

describe("ProviderSetupCard", () => {
  it("opens each way to connect on its own provider card", () => {
    const onSetUp = vi.fn();
    render(<ProviderSetupCard onSetUp={onSetUp} />);

    fireEvent.click(
      screen.getByRole("button", { name: /Sign in with ChatGPT/ }),
    );
    expect(onSetUp).toHaveBeenLastCalledWith({
      provider: "openai",
      focusCredential: false,
    });

    fireEvent.click(screen.getByRole("button", { name: /Anthropic/ }));
    expect(onSetUp).toHaveBeenLastCalledWith({
      provider: "anthropic",
      focusCredential: true,
    });

    fireEvent.click(screen.getByRole("button", { name: /Connect Ollama/ }));
    expect(onSetUp).toHaveBeenLastCalledWith({
      provider: "ollama",
      focusCredential: false,
    });

    fireEvent.click(
      screen.getByRole("button", {
        name: /Connect an OpenAI-compatible server/,
      }),
    );
    expect(onSetUp).toHaveBeenLastCalledWith({
      provider: "openai_compatible",
      focusCredential: false,
    });

    fireEvent.click(screen.getByRole("button", { name: "More providers" }));
    expect(onSetUp).toHaveBeenLastCalledWith({
      provider: null,
      focusCredential: false,
    });
  });

  it("stands where the starters go, and the walkthrough waits", () => {
    render(
      <WelcomeState
        onSelectPrompt={vi.fn()}
        onStartWalkthrough={vi.fn()}
        heading="Connect a model to start"
        setup={<ProviderSetupCard onSetUp={vi.fn()} />}
      />,
    );

    expect(
      screen.getByRole("navigation", { name: "Connect a model" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Brief this week's AI news/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Set up your first task" }),
    ).not.toBeInTheDocument();
  });
});

function BlockedComposer({
  blocker,
  onSend,
}: {
  blocker: ComposerSendBlocker | null;
  onSend: () => Promise<void>;
}) {
  const [draft, setDraft] = useState("Summarize this week's AI news");
  return (
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft={draft}
      onDraftChange={setDraft}
      onSend={onSend}
      onSteer={vi.fn(async () => {})}
      onStop={vi.fn(async () => {})}
      resetKey="home"
      steerError={null}
      steerPending={false}
      steerStatus={null}
      sendBlocker={blocker}
    />
  );
}

describe("a composer that cannot send", () => {
  it("keeps send off with the reason and the fix beside it", () => {
    const onSend = vi.fn(async () => {});
    const onClick = vi.fn();
    render(
      <BlockedComposer
        blocker={{
          reason: "Connect a model provider to send.",
          action: { label: "Set up a provider", onClick },
        }}
        onSend={onSend}
      />,
    );

    const send = screen.getByRole("button", { name: "Send message" });
    expect(send).toBeDisabled();
    expect(send).toHaveAccessibleDescription(
      /Connect a model provider to send\./,
    );
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Message" }), {
      key: "Enter",
    });
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Set up a provider" }));
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("sends as before once a model can run", () => {
    render(<BlockedComposer blocker={null} onSend={vi.fn(async () => {})} />);

    expect(screen.getByRole("button", { name: "Send message" })).toBeEnabled();
    expect(
      screen.queryByText("Connect a model provider to send."),
    ).not.toBeInTheDocument();
  });
});

describe("while no model can run", () => {
  // Review finding: a ChatGPT sign-in that finished after its Settings panel
  // closed, or a gateway model list that landed after its sign-in, left the
  // send block on until a restart.
  it("reads the catalog again on focus and on a timer, and stops once a model can run", async () => {
    vi.useFakeTimers();
    try {
      const refresh = vi.fn(async () => {});
      const { rerender, unmount } = renderHook(
        ({ blocked }) => useRefreshWhileNoModelRuns(blocked, refresh, 1_000),
        { initialProps: { blocked: true } },
      );

      window.dispatchEvent(new Event("focus"));
      await vi.advanceTimersByTimeAsync(0);
      expect(refresh).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(refresh).toHaveBeenCalledTimes(2);

      rerender({ blocked: false });
      await vi.advanceTimersByTimeAsync(5_000);
      window.dispatchEvent(new Event("focus"));
      expect(refresh).toHaveBeenCalledTimes(2);
      unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("never runs a second read while one is in flight", async () => {
    vi.useFakeTimers();
    try {
      let finish: () => void = () => {};
      const refresh = vi.fn(
        () =>
          new Promise<void>((resolve) => {
            finish = resolve;
          }),
      );
      const { unmount } = renderHook(() =>
        useRefreshWhileNoModelRuns(true, refresh, 1_000),
      );

      await vi.advanceTimersByTimeAsync(3_000);
      expect(refresh).toHaveBeenCalledTimes(1);
      finish();
      await vi.advanceTimersByTimeAsync(1_000);
      expect(refresh).toHaveBeenCalledTimes(2);
      unmount();
    } finally {
      vi.useRealTimers();
    }
  });
});
