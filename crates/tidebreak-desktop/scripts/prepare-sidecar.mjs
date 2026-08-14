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
const targetRoot = process.env.CARGO_TARGET_DIR
  ? resolve(workspaceDir, process.env.CARGO_TARGET_DIR)
  : join(workspaceDir, "target");
const destinationDir = join(desktopDir, "binaries");
mkdirSync(destinationDir, { recursive: true });

// Tauri's synthetic universal target builds the app once per real Rust target
// and lipo-combines the app executable. Its bundler expects an already-combined
// external binary under the synthetic target name, while the per-target Cargo
// builds still need their own target-suffixed sidecars.
const targets =
  triple === "universal-apple-darwin"
    ? ["aarch64-apple-darwin", "x86_64-apple-darwin"]
    : [triple];
const stagedSidecars = [];

for (const target of targets) {
  const extension = target.includes("windows") ? ".exe" : "";
  const cargoArgs = [
    "build",
    "-p",
    "tidebreak-host-broker",
    "--bin",
    "tidebreak-host-broker",
    "--locked",
  ];
  if (release) cargoArgs.push("--release");
  if (configuredTarget) cargoArgs.push("--target", target);
  execFileSync("cargo", cargoArgs, { cwd: workspaceDir, stdio: "inherit" });

  const source = join(
    targetRoot,
    ...(configuredTarget ? [target] : []),
    profile,
    `tidebreak-host-broker${extension}`,
  );
  const destination = join(
    destinationDir,
    `tidebreak-host-broker-${target}${extension}`,
  );

  copyFileSync(source, destination);
  if (process.platform !== "win32") chmodSync(destination, 0o755);
  stagedSidecars.push(destination);
}

if (triple === "universal-apple-darwin") {
  const destination = join(
    destinationDir,
    "tidebreak-host-broker-universal-apple-darwin",
  );
  execFileSync(
    "lipo",
    ["-create", ...stagedSidecars, "-output", destination],
    { stdio: "inherit" },
  );
  chmodSync(destination, 0o755);
}
