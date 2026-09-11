// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { CodeSessionSnapshot } from "../api/types";
import { codeDigest, codeSession } from "@/stories/fixtures";
import { useCodeUpdatesStore, useSessionDigest } from "./CodeUpdatesStore";
import { SessionRecoveryNotice } from "./SessionRecoveryNotice";
import { sessionRecoveryState } from "./sessionRecovery";

afterEach(() => {
  cleanup();
  useCodeUpdatesStore.getState().resetLive();
});

function SelectedSession({
  session,
  onRetry,
}: {
  session: CodeSessionSnapshot;
  onRetry: () => void;
}) {
  const digest = useSessionDigest(
    session.workspace_id ?? undefined,
    session.id,
  );
  const state = sessionRecoveryState(session, digest);
  return (
    <>
      <textarea aria-label="Message" disabled={state.blocksTurn} />
      <SessionRecoveryNotice
        {...state}
        showProgress={false}
        onRetry={onRetry}
      />
    </>
  );
}

it.each(["interactive", "watch"] as const)(
  "keeps recovery scoped to the selected session beside a %s sibling",
  (kind) => {
    const retry = vi.fn();
    const healthy = {
      ...codeSession,
      id: "healthy",
      lifecycle: "idle" as const,
      attention: {
        state: { type: "idle" as const },
        source: "lifecycle" as const,
      },
    };
    const fenced = {
      ...codeSession,
      id: "fenced",
      kind,
      lifecycle: "fenced" as const,
      fence_reason: { type: "orphan_alive" as const },
    };
    useCodeUpdatesStore.getState().apply({
      type: "snapshot",
      sessions: [
        codeDigest({
          session: healthy.id,
          lifecycle: healthy.lifecycle,
          attention: healthy.attention,
        }),
        codeDigest({
          session: fenced.id,
          kind,
          lifecycle: "fenced",
          fence_reason: { type: "probe_ambiguous", detail: "Process changed" },
          attention: {
            state: {
              type: "needs_you",
              prompt: "Inspect the fenced session's process.",
              source: "lifecycle",
            },
            source: "lifecycle",
          },
        }),
      ],
    });
    const view = render(<SelectedSession session={healthy} onRetry={retry} />);
    expect(screen.getByRole("textbox", { name: "Message" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Retry recovery" })).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
    expect(retry).not.toHaveBeenCalled();

    view.rerender(<SelectedSession session={fenced} onRetry={retry} />);
    expect(screen.getByRole("textbox", { name: "Message" })).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent(
      "Inspect the fenced session's process.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Retry recovery" }));
    expect(retry).toHaveBeenCalledOnce();

    view.rerender(<SelectedSession session={healthy} onRetry={retry} />);
    expect(screen.getByRole("textbox", { name: "Message" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Retry recovery" })).toBeNull();
  },
);
