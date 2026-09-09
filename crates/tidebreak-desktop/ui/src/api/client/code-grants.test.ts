import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiClient, HttpError } from "../../api";

afterEach(() => vi.unstubAllGlobals());

describe("channel repository approval", () => {
  it("encodes both path segments, sends explicit repositories, and accepts 204", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://localhost", "test-token");
    await expect(
      client.approveWorkspaceGrantChannelRepositories("grant/1", "C/#1", [
        "acme/tools",
        "https://github.com/acme/web",
      ]),
    ).resolves.toBeUndefined();
    expect(fetchMock).toHaveBeenCalledExactlyOnceWith(
      "http://localhost/deployment/code/grants/workspace/grant%2F1/channels/C%2F%231/repositories/approve",
      {
        method: "POST",
        headers: {
          Authorization: "Bearer test-token",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          repositories: ["acme/tools", "https://github.com/acme/web"],
        }),
      },
    );
  });

  it("preserves an admin-only authorization failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          new Response(
            JSON.stringify({ message: "administrator access required" }),
            { status: 403 },
          ),
        ),
    );
    const client = new ApiClient("http://localhost", "test-token");
    await expect(
      client.approveWorkspaceGrantChannelRepositories("grant-1", "C1", [
        "acme/tools",
      ]),
    ).rejects.toEqual(new HttpError(403, "403: administrator access required"));
  });
});
