// @vitest-environment jsdom
import { describe, expect, it } from "vitest";

import { monacoLanguage, monacoTheme } from "./monacoEnv";

describe("monaco environment", () => {
  it("selects syntax grammars from extensions and well-known filenames", () => {
    expect(monacoLanguage("src/main.rs")).toBe("rust");
    expect(monacoLanguage("ui/CodeWorkspacePage.tsx")).toBe("typescript");
    expect(monacoLanguage("Dockerfile")).toBe("dockerfile");
    expect(monacoLanguage("scripts/release.zsh")).toBe("shell");
    expect(monacoLanguage("assets/blob.unknown")).toBe("plaintext");
  });

  it("maps live app themes to Monaco themes", () => {
    expect(monacoTheme("light")).toBe("vs");
    expect(monacoTheme("dark")).toBe("vs-dark");
  });
});
