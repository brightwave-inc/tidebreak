import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeSubscriptionUsage } from "../api/types";
import { resetCodeClientGenerationForTests } from "./CodeClientGeneration";
import { activateCodeClient } from "./CodeClientScope";
import {
  resetCodeSubscriptionUsageStore,
  useCodeSubscriptionUsageStore,
} from "./useCodeSubscriptionUsage";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function usage(providerId: string): CodeSubscriptionUsage {
  return {
    source: "model_gateway",
    diagnostics: [],
    providers: [
      {
        id: providerId,
        label: providerId,
        accounts: [],
      },
    ],
  };
}

afterEach(() => {
  resetCodeSubscriptionUsageStore();
  resetCodeClientGenerationForTests();
});

describe("Code subscription usage client generation", () => {
  it("starts the replacement refresh and ignores the old result", async () => {
    const staleUsage = deferred<CodeSubscriptionUsage>();
    const first = {
      getCodeSubscriptionUsage: vi.fn(() => staleUsage.promise),
    };
    activateCodeClient(first);
    const stale = useCodeSubscriptionUsageStore
      .getState()
      .refresh(first as never);

    const freshUsage = deferred<CodeSubscriptionUsage>();
    const replacement = {
      getCodeSubscriptionUsage: vi.fn(() => freshUsage.promise),
    };
    activateCodeClient(replacement);
    const fresh = useCodeSubscriptionUsageStore
      .getState()
      .refresh(replacement as never);

    await Promise.resolve();
    expect(replacement.getCodeSubscriptionUsage).toHaveBeenCalledOnce();
    staleUsage.resolve(usage("old"));
    await stale;
    expect(useCodeSubscriptionUsageStore.getState()).toMatchObject({
      report: null,
      refreshing: true,
      refreshInFlight: true,
    });

    freshUsage.resolve(usage("new"));
    await fresh;
    expect(useCodeSubscriptionUsageStore.getState()).toMatchObject({
      report: usage("new"),
      refreshing: false,
      refreshInFlight: false,
      error: null,
    });
  });
});
