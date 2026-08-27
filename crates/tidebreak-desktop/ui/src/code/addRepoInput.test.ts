import { describe, expect, it } from "vitest";

import {
  addRepoCloneRequest,
  addRepoInputClones,
  classifyAddRepoInput,
} from "./addRepoInput";

describe("classifyAddRepoInput", () => {
  it("reads a path, a remote, and a GitHub slug out of one field", () => {
    expect(classifyAddRepoInput("")).toBe("empty");
    expect(classifyAddRepoInput("   ")).toBe("empty");
    expect(classifyAddRepoInput("/Users/sam/src/app")).toBe("path");
    expect(classifyAddRepoInput("~/src/app")).toBe("path");
    expect(classifyAddRepoInput("./app")).toBe("path");
    expect(classifyAddRepoInput("../app")).toBe("path");
    expect(classifyAddRepoInput("https://example.com/acme/app.git")).toBe(
      "url",
    );
    expect(classifyAddRepoInput("ssh://git@example.com/acme/app")).toBe("url");
    expect(classifyAddRepoInput("git@github.com:acme/app.git")).toBe("url");
    expect(classifyAddRepoInput("acme/app")).toBe("github");
  });

  it("keeps a Windows path off the scp-remote branch", () => {
    // `C:\src\app` matches Git's `user@host:path` shape closely enough that a
    // looser rule would try to clone a drive letter.
    expect(classifyAddRepoInput("C:\\Users\\sam\\app")).toBe("path");
    expect(classifyAddRepoInput("C:/Users/sam/app")).toBe("path");
  });

  it("sends anything it cannot place to the machine as a path", () => {
    // `createCodeRepo` answers with the reason that path is not a repository,
    // which beats any guess this function could make.
    expect(classifyAddRepoInput("app")).toBe("path");
    expect(classifyAddRepoInput("acme/app/extra")).toBe("path");
  });

  it("knows which kinds need somewhere to clone into", () => {
    expect(addRepoInputClones("url")).toBe(true);
    expect(addRepoInputClones("github")).toBe(true);
    expect(addRepoInputClones("path")).toBe(false);
    expect(addRepoInputClones("empty")).toBe(false);
  });
});

describe("addRepoCloneRequest", () => {
  it("sends a URL under `url` and a slug under `github`", () => {
    expect(
      addRepoCloneRequest({
        value: " https://example.com/acme/app.git ",
        parentDir: "/tmp/src",
        machineChoosesDestination: false,
      }),
    ).toEqual({
      url: "https://example.com/acme/app.git",
      parent_dir: "/tmp/src",
    });
    expect(
      addRepoCloneRequest({
        value: "acme/app",
        parentDir: "/tmp/src",
        machineChoosesDestination: false,
      }),
    ).toEqual({ github: "acme/app", parent_dir: "/tmp/src" });
  });

  it("omits a destination the machine chooses for itself", () => {
    // The field was never shown, so whatever a defaults read left behind must
    // not decide where the checkout lands.
    expect(
      addRepoCloneRequest({
        value: "acme/app",
        parentDir: "/tmp/stale",
        machineChoosesDestination: true,
      }),
    ).toEqual({ github: "acme/app", parent_dir: undefined });
  });

  it("refuses a clone with nowhere to go, and a path that never clones", () => {
    expect(
      addRepoCloneRequest({
        value: "acme/app",
        parentDir: "  ",
        machineChoosesDestination: false,
      }),
    ).toBeNull();
    expect(
      addRepoCloneRequest({
        value: "/Users/sam/app",
        parentDir: "/tmp/src",
        machineChoosesDestination: false,
      }),
    ).toBeNull();
  });
});
