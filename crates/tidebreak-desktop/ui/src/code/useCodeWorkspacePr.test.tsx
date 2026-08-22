// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { CodeWorkspacePrSnapshot, PullRequestDigest } from "../api/types";
import { useCodeWorkspacePr } from "./useCodeWorkspacePr";

const CLEAN: CodeWorkspacePrSnapshot = {
  dirty: false,
  unpushed: false,
  ahead: 0,
  has_upstream: true,
  suggested_commit_message: "",
  gh_found: true,
  gh_authenticated: true,
  remediation: "",
};

function pr(number: number): PullRequestDigest {
  return { number, state: "open" };
}

describe("useCodeWorkspacePr", () => {
  it("refreshes the complete snapshot when the live PR digest changes", async () => {
    const first = { ...CLEAN, pr: pr(41) };
    const second = { ...CLEAN, pr: pr(42) };
    const getCodeWorkspacePr = vi
      .fn()
      .mockResolvedValueOnce(first)
      .mockResolvedValueOnce(second);

    const { result, rerender } = renderHook(
      ({ livePr }: { livePr?: PullRequestDigest }) =>
        useCodeWorkspacePr({ getCodeWorkspacePr }, "workspace-1", 0, livePr),
      { initialProps: { livePr: first.pr } },
    );

    await waitFor(() => expect(result.current.data).toEqual(first));
    rerender({ livePr: second.pr });

    await waitFor(() => expect(result.current.data).toEqual(second));
    expect(getCodeWorkspacePr).toHaveBeenCalledTimes(2);
  });

  it("does not reload for an equal live PR digest with a new object identity", async () => {
    const snapshot = { ...CLEAN, pr: pr(41) };
    const getCodeWorkspacePr = vi.fn(async () => snapshot);
    const { result, rerender } = renderHook(
      ({ livePr }: { livePr?: PullRequestDigest }) =>
        useCodeWorkspacePr({ getCodeWorkspacePr }, "workspace-1", 0, livePr),
      { initialProps: { livePr: snapshot.pr } },
    );

    await waitFor(() => expect(result.current.data).toEqual(snapshot));
    rerender({ livePr: { ...snapshot.pr } });
    await new Promise((resolve) => window.setTimeout(resolve, 300));

    expect(getCodeWorkspacePr).toHaveBeenCalledOnce();
  });

  it("does not let an unchanged stale digest overwrite an adopted result", async () => {
    const stale = { ...CLEAN, pr: pr(41) };
    const adopted = { ...CLEAN, pr: pr(42) };
    const getCodeWorkspacePr = vi.fn(async () => stale);

    const { result } = renderHook(() =>
      useCodeWorkspacePr({ getCodeWorkspacePr }, "workspace-1", 0, stale.pr),
    );

    await waitFor(() => expect(result.current.data).toEqual(stale));
    act(() => result.current.adopt(adopted));

    expect(result.current.data).toEqual(adopted);
    expect(getCodeWorkspacePr).toHaveBeenCalledTimes(1);
  });

  it("serializes mutations shared by the header and source-control view", async () => {
    const getCodeWorkspacePr = vi.fn(async () => CLEAN);
    let finish!: () => void;
    const operation = vi.fn(
      () =>
        new Promise<string>((resolve) => {
          finish = () => resolve("done");
        }),
    );
    const { result } = renderHook(() =>
      useCodeWorkspacePr({ getCodeWorkspacePr }, "workspace-1", 0),
    );
    await waitFor(() => expect(result.current.data).toEqual(CLEAN));

    let first!: Promise<string | undefined>;
    act(() => {
      first = result.current.runMutation("push", operation);
    });
    expect(result.current.busy).toBe("push");
    await expect(
      result.current.runMutation("create_pr", async () => "duplicate"),
    ).resolves.toBeUndefined();
    expect(operation).toHaveBeenCalledOnce();

    await act(async () => {
      finish();
      await first;
    });
    expect(result.current.busy).toBeNull();
  });

  it("serializes a forced host refresh with mutations", async () => {
    let finishRefresh!: (value: CodeWorkspacePrSnapshot) => void;
    const refreshCodeWorkspacePr = vi.fn(
      () =>
        new Promise<CodeWorkspacePrSnapshot>((resolve) => {
          finishRefresh = resolve;
        }),
    );
    const getCodeWorkspacePr = vi.fn(async () => CLEAN);
    const mutation = vi.fn(async () => "merged");
    const { result } = renderHook(() =>
      useCodeWorkspacePr(
        { getCodeWorkspacePr, refreshCodeWorkspacePr },
        "workspace-1",
        0,
      ),
    );
    await waitFor(() => expect(result.current.data).toEqual(CLEAN));

    let refresh!: Promise<CodeWorkspacePrSnapshot | undefined>;
    act(() => {
      refresh = result.current.refreshFromHost();
    });
    expect(result.current.busy).toBe("refresh");
    await expect(
      result.current.runMutation("merge", mutation),
    ).resolves.toBeUndefined();
    expect(mutation).not.toHaveBeenCalled();

    const refreshed = { ...CLEAN, pr: pr(42) };
    await act(async () => {
      finishRefresh(refreshed);
      await refresh;
    });
    expect(result.current.data).toEqual(refreshed);
    expect(result.current.busy).toBeNull();
  });

  it("does not let an older passive load overwrite a forced host refresh", async () => {
    let finishPassive!: (value: CodeWorkspacePrSnapshot) => void;
    const getCodeWorkspacePr = vi.fn(
      () =>
        new Promise<CodeWorkspacePrSnapshot>((resolve) => {
          finishPassive = resolve;
        }),
    );
    const fresh = { ...CLEAN, pr: pr(42) };
    const refreshCodeWorkspacePr = vi.fn(async () => fresh);
    const { result } = renderHook(() =>
      useCodeWorkspacePr(
        { getCodeWorkspacePr, refreshCodeWorkspacePr },
        "workspace-1",
        0,
      ),
    );
    await waitFor(() => expect(getCodeWorkspacePr).toHaveBeenCalledOnce());

    await act(async () => {
      await result.current.refreshFromHost();
    });
    expect(result.current.data).toEqual(fresh);

    await act(async () => {
      finishPassive({ ...CLEAN, pr: pr(41) });
      await Promise.resolve();
    });
    expect(result.current.data).toEqual(fresh);
  });

  it("ignores an old mutation error after the next workspace loads", async () => {
    const first = { ...CLEAN, pr: pr(41) };
    const second = { ...CLEAN, pr: pr(42) };
    const getCodeWorkspacePr = vi.fn(async (workspaceId: string) =>
      workspaceId === "workspace-1" ? first : second,
    );
    let rejectMutation!: (error: Error) => void;
    const operation = () =>
      new Promise<void>((_resolve, reject) => {
        rejectMutation = reject;
      });
    const { result, rerender } = renderHook(
      ({ workspaceId }: { workspaceId: string }) =>
        useCodeWorkspacePr({ getCodeWorkspacePr }, workspaceId, 0),
      { initialProps: { workspaceId: "workspace-1" } },
    );
    await waitFor(() => expect(result.current.data).toEqual(first));

    const oldSetMutationError = result.current.setMutationError;
    let mutation!: Promise<void | undefined>;
    act(() => {
      mutation = result.current
        .runMutation("push", operation)
        .catch((error: Error) => {
          oldSetMutationError(error.message);
          return undefined;
        });
    });
    rerender({ workspaceId: "workspace-2" });
    await waitFor(() => expect(result.current.data).toEqual(second));
    expect(result.current.busy).toBeNull();

    await act(async () => {
      rejectMutation(new Error("old push failed"));
      await mutation;
    });

    expect(result.current.data).toEqual(second);
    expect(result.current.mutationError).toBeNull();
  });
});
