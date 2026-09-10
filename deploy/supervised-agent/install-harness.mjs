import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const engines = [
  { kind: "ClaudeCode", package: "@anthropic-ai/claude-code", binary: "claude" },
  { kind: "Codex", package: "@openai/codex", binary: "codex" },
];

// Use the same exact package versions that Tidebreak probes and tests.
export function workloadHarnessPins(source) {
  const found = [...source.matchAll(/kind: HarnessKind::(ClaudeCode|Codex),\s*version: "([0-9]+\.[0-9]+\.[0-9]+)",\s*package: "([^"]+)"/g)];
  return engines.map((engine) => {
    const matches = found.filter((match) => match[1] === engine.kind);
    if (matches.length !== 1 || matches[0][3] !== engine.package) {
      throw new Error(`The ${engine.kind} package pin is missing or malformed.`);
    }
    return { ...engine, version: matches[0][2] };
  });
}

export function installWorkloadHarnesses(source, npmPath, run = execFileSync) {
  // Validate every pin before installing any package.
  const pins = workloadHarnessPins(source);
  run(npmPath, ["install", "--global", "--prefix", "/usr/local", "--omit=dev", "--no-fund", "--no-audit", "--no-progress", ...pins.map((pin) => `${pin.package}@${pin.version}`)], { stdio: "inherit" });
  for (const pin of pins) {
    const actual = run(`/usr/local/bin/${pin.binary}`, ["--version"], { encoding: "utf8" }).trim();
    const expected = pin.kind === "Codex" ? `codex-cli ${pin.version}` : `${pin.version} (Claude Code)`;
    if (actual !== expected) {
      throw new Error(`The installed ${pin.kind} version does not match Tidebreak's pin.`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [pinsPath, npmPath] = process.argv.slice(2);
  installWorkloadHarnesses(readFileSync(pinsPath, "utf8"), npmPath);
}
