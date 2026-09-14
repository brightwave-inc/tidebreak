// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { ApiClient } from "./client";
const preferences = {
  dm_subscription_preference: "prefer_owned_subscription",
  channel_sponsorship_enabled: false,
  consent_version: null,
  inference_sponsorship_supported: true,
};
function stubResponse(value: unknown = {}) {
  const fetch = vi.fn(
    async () =>
      new Response(JSON.stringify(value), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
  );
  vi.stubGlobal("fetch", fetch);
  return fetch;
}
afterEach(() => vi.unstubAllGlobals());
it("loads and saves preferences through the owner's grant endpoint", async () => {
  const fetch = stubResponse(preferences);
  const client = new ApiClient("http://127.0.0.1", "token");
  expect(await client.getPersonalInferencePreferences("mine/one")).toEqual(
    preferences,
  );
  expect(fetch).toHaveBeenLastCalledWith(
    "http://127.0.0.1/code/grants/mine%2Fone/inference-preferences",
    expect.objectContaining({ headers: expect.anything() }),
  );
  const update = {
    dm_subscription_preference: "gateway_default" as const,
    channel_sponsorship_enabled: true,
    consent_version: 1 as const,
  };
  await client.setPersonalInferencePreferences("mine/one", update);
  expect(fetch).toHaveBeenLastCalledWith(
    "http://127.0.0.1/code/grants/mine%2Fone/inference-preferences",
    expect.objectContaining({ method: "PUT", body: JSON.stringify(update) }),
  );
});
it("omits sponsorship on legacy approval and sends only the explicit choice on supported servers", async () => {
  const fetch = stubResponse();
  const client = new ApiClient("http://127.0.0.1", "token");
  await client.approveCodeConnect("nonce/one", "csrf");
  expect(fetch).toHaveBeenLastCalledWith(
    "http://127.0.0.1/external/connect/nonce%2Fone/approve",
    expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ csrf: "csrf" }),
    }),
  );
  await client.approveCodeConnect("nonce/one", "csrf", { enabled: false });
  expect(fetch).toHaveBeenLastCalledWith(
    expect.anything(),
    expect.objectContaining({
      body: JSON.stringify({
        csrf: "csrf",
        inference_sponsorship: { enabled: false },
      }),
    }),
  );
  await client.approveCodeConnect("nonce/one", "csrf", {
    enabled: true,
    consent_version: 1,
  });
  expect(fetch).toHaveBeenLastCalledWith(
    expect.anything(),
    expect.objectContaining({
      body: JSON.stringify({
        csrf: "csrf",
        inference_sponsorship: { enabled: true, consent_version: 1 },
      }),
    }),
  );
});
