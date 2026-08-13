import { describe, expect, it } from "vitest";
import os from "node:os";
import path from "node:path";

import {
  candidateListenFiles,
  DEV_APP_IDENTIFIER,
  parseListenFile,
} from "./vite-dev-listen";

describe("parseListenFile", () => {
  it("reads the desktop listen endpoint", () => {
    expect(
      parseListenFile('{"base_url":"http://127.0.0.1:54321/","token":"abc"}'),
    ).toEqual({ baseUrl: "http://127.0.0.1:54321", token: "abc" });
  });

  it("rejects a missing token", () => {
    expect(parseListenFile('{"base_url":"http://127.0.0.1:9"}')).toBeNull();
  });
});

describe("candidateListenFiles", () => {
  it("prefers TIDEBREAK_DATA_DIR", () => {
    const files = candidateListenFiles(
      { TIDEBREAK_DATA_DIR: "/tmp/tide-profile" },
      "/Users/dev",
    );
    expect(files[0]).toBe(path.join("/tmp/tide-profile", "listen.json"));
  });

  it("includes the debug desktop profile", () => {
    const files = candidateListenFiles({}, os.homedir());
    expect(files.some((file) => file.includes(DEV_APP_IDENTIFIER))).toBe(true);
  });
});
