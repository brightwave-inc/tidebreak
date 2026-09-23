import { describe, expect, it } from "vitest";

import { HttpError } from "@/api";
import {
  parseInstallOutcome,
  plainGitInstallError,
  validateGitInstallInput,
} from "./gitInstall";

describe("git plugin install helpers", () => {
  it("requires https and a pinned revision", () => {
    expect(validateGitInstallInput("", "v1")).toMatch(/https/);
    expect(
      validateGitInstallInput("http://github.com/acme/notes", "v1"),
    ).toMatch(/https/);
    expect(
      validateGitInstallInput("https://github.com/acme/notes", "  "),
    ).toMatch(/tag or a full commit SHA/);
    expect(
      validateGitInstallInput("https://github.com/acme/notes", "v1.0.0"),
    ).toBeNull();
  });

  it("maps server kinds into short sentences", () => {
    expect(
      plainGitInstallError(
        new HttpError(400, "not https", "plugin_source_invalid"),
      ),
    ).toBe("Use an https repository URL.");
    expect(
      plainGitInstallError(
        new HttpError(422, "did not resolve", "plugin_source_unavailable"),
      ),
    ).toBe("Could not reach that host.");
    expect(
      plainGitInstallError(new HttpError(422, "no plugin", "plugin_invalid")),
    ).toBe("No recognizable plugin was found in that repository.");
    expect(
      plainGitInstallError(new HttpError(409, "exists", "plugin_conflict")),
    ).toBe("A plugin with that name is already installed.");
  });

  it("reads skipped members from the install response", () => {
    expect(
      parseInstallOutcome({
        plugin: "notes",
        revision: "v1",
        skipped: [{ path: "skills/x/SKILL.md", reason: "bad name" }],
      }),
    ).toEqual({
      plugin: "notes",
      revision: "v1",
      skipped: [{ path: "skills/x/SKILL.md", reason: "bad name" }],
    });
  });
});
