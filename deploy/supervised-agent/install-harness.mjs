import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";

// Read the same exact version that Tidebreak probes and tests.
const [pinsPath, npmPath] = process.argv.slice(2);
const pins = readFileSync(pinsPath, "utf8");
const pin = pins.match(/kind: HarnessKind::ClaudeCode,\s*version: "([0-9]+\.[0-9]+\.[0-9]+)",\s*package: "(@anthropic-ai\/claude-code)"/);
if (!pin) throw new Error("The Claude Code package pin is missing or malformed.");
const [, version, name] = pin;
execFileSync(npmPath, ["install", "--global", "--prefix", "/usr/local", "--omit=dev", "--no-fund", "--no-audit", "--no-progress", name + "@" + version], { stdio: "inherit" });
const actual = execFileSync("/usr/local/bin/claude", ["--version"], { encoding: "utf8" }).trim();
if (!actual.startsWith(version + " ")) throw new Error("The installed Claude Code version does not match Tidebreak's pin.");
