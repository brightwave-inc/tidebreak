import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const desktopDir = resolve(scriptDir, "..");
const workspaceDir = resolve(desktopDir, "../..");
const release = process.argv.includes("--release");
const profile = release ? "release" : "debug";
const configuredTarget = process.env.TAURI_ENV_TARGET_TRIPLE?.trim();
const triple =
  configuredTarget ||
  execFileSync("rustc", ["--print", "host-tuple"], {
    encoding: "utf8",
  }).trim();

if (!triple) {
  throw new Error("rustc did not report a host target triple");
}
const extension = triple.includes("windows") ? ".exe" : "";

const cargoArgs = [
  "build",
  "-p",
  "openwave-host-broker",
  "--bin",
  "openwave-host-broker",
];
if (release) cargoArgs.push("--release");
if (configuredTarget) cargoArgs.push("--target", configuredTarget);
execFileSync("cargo", cargoArgs, { cwd: workspaceDir, stdio: "inherit" });

const targetRoot = process.env.CARGO_TARGET_DIR
  ? resolve(workspaceDir, process.env.CARGO_TARGET_DIR)
  : join(workspaceDir, "target");
const source = join(
  targetRoot,
  ...(configuredTarget ? [configuredTarget] : []),
  profile,
  `openwave-host-broker${extension}`,
);
const destinationDir = join(desktopDir, "binaries");
const destination = join(
  destinationDir,
  `openwave-host-broker-${triple}${extension}`,
);

mkdirSync(destinationDir, { recursive: true });
copyFileSync(source, destination);
if (process.platform !== "win32") chmodSync(destination, 0o755);
