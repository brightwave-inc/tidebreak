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
// builds still need their own target-suffixed sidecars. Release CI can instead
// invoke this hook once per real target on parallel runners, then lipo the two
// staged sidecars alongside the two desktop binaries before `tauri bundle`.
const targets =
  triple === "universal-apple-darwin"
    ? ["aarch64-apple-darwin", "x86_64-apple-darwin"]
    : [triple];

/**
 * Build and stage one named Cargo binary into the Tauri sidecar directory.
 *
 */
function stageBinary(binaryName, packageName) {
  const stagedSidecars = [];
  for (const target of targets) {
    const extension = target.includes("windows") ? ".exe" : "";
    const cargoArgs = [
      "build",
      "-p",
      packageName,
      "--bin",
      binaryName,
      "--locked",
    ];
    if (release) cargoArgs.push("--release");
    if (configuredTarget) cargoArgs.push("--target", target);
    execFileSync("cargo", cargoArgs, { cwd: workspaceDir, stdio: "inherit" });

    const source = join(
      targetRoot,
      ...(configuredTarget ? [target] : []),
      profile,
      `${binaryName}${extension}`,
    );
    const destination = join(
      destinationDir,
      `${binaryName}-${target}${extension}`,
    );

    copyFileSync(source, destination);
    if (process.platform !== "win32") chmodSync(destination, 0o755);
    stagedSidecars.push(destination);
  }

  if (triple === "universal-apple-darwin") {
    const destination = join(
      destinationDir,
      `${binaryName}-universal-apple-darwin`,
    );
    execFileSync(
      "lipo",
      ["-create", ...stagedSidecars, "-output", destination],
      { stdio: "inherit" },
    );
    chmodSync(destination, 0o755);
  }
}

// The host broker is the desktop's existing sidecar: it owns the per-workspace
// process tree for code executions.
stageBinary("tidebreak-host-broker", "tidebreak-host-broker");

// The CLI binary gives every Tidebreak desktop session a canonical `tidebreak`
// command on PATH so provider harnesses (Claude, Codex, OpenCode) can invoke
// `tidebreak browser-mcp` or other agent-side commands through a session-
// scoped capfile without asking the user to install or find anything.
// The later harness command-path PR resolves the absolute path at runtime;
// this slice only packages the binary so it is available on disk.
stageBinary("tidebreak", "tidebreak-cli");
