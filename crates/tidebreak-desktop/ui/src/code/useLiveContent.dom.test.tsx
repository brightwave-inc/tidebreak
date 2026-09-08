// @vitest-environment jsdom
import { act, renderHook, waitFor } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { useLiveResource } from "./useLiveContent";

it("retires an in-flight read when disabled and reloads when enabled", async () => {
  let finish: (value: string) => void = () => {};
  const load = vi
    .fn()
    .mockImplementationOnce(
      () =>
        new Promise<string>((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue("fresh");
  const { result, rerender } = renderHook(
    ({ enabled, revision }) =>
      useLiveResource({
        key: "workspace",
        revision,
        enabled,
        load,
        errorMessage: "Read failed",
      }),
    { initialProps: { enabled: true, revision: 0 } },
  );
  expect(load).toHaveBeenCalledOnce();
  rerender({ enabled: false, revision: 1 });
  await act(async () => {
    finish("stale");
  });
  expect(result.current.data).toBeNull();
  expect(result.current.refreshing).toBe(false);
  await act(async () => {
    await result.current.refresh();
  });
  expect(load).toHaveBeenCalledOnce();
  rerender({ enabled: true, revision: 1 });
  await waitFor(() => expect(result.current.data).toBe("fresh"));
});
