import { describe, expect, it } from "vitest";

import { railAccountIdentity } from "./railAccountIdentity";

describe("railAccountIdentity", () => {
  it("stays local when nothing is signed in", () => {
    expect(
      railAccountIdentity({
        gateway: { signed_in: false },
      }),
    ).toEqual({
      title: "Account",
      detail: null,
      githubLogin: undefined,
      source: "local",
    });
  });

  it("uses a gateway hint, and a GitHub login as the face when both exist", () => {
    expect(
      railAccountIdentity({
        gateway: { signed_in: true, account_hint: "abaas@example.test" },
      }),
    ).toEqual({
      title: "abaas",
      detail: "abaas@example.test",
      githubLogin: undefined,
      source: "gateway",
    });
    expect(
      railAccountIdentity({
        gateway: { signed_in: true, account_hint: "abaas@example.test" },
        githubLogin: "thet",
      }),
    ).toEqual({
      title: "thet",
      detail: "abaas@example.test",
      githubLogin: "thet",
      source: "gateway",
    });
  });

  it("names a GitHub login when the gateway is empty", () => {
    expect(
      railAccountIdentity({
        gateway: null,
        githubLogin: "mira-chen",
      }),
    ).toEqual({
      title: "mira-chen",
      detail: null,
      githubLogin: "mira-chen",
      source: "github",
    });
  });
});
