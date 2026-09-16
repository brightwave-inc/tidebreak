import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { stageComputerUseHelper } from "./prepare-computer-use-helper.mjs";

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

/**
 * The environment Cargo runs under for the sidecar build.
 *
 * Tauri exports `MACOSX_DEPLOYMENT_TARGET` from `bundle.macOS.minimumSystemVersion`
 * when it compiles the desktop crate. The build scripts of `ring`, `aws-lc-sys`,
 * `libsqlite3-sys`, and `objc2-exception-helper` declare that variable as an
 * input, so a sidecar build that ran without it leaves every crate above them,
 * tidebreak-core included, dirty for Tauri's pass. Setting the same value here
 * keeps the two passes' fingerprints identical on macOS.
 */
function cargoEnvironment() {
  const env = { ...process.env };
  if (triple.includes("apple") && !env.MACOSX_DEPLOYMENT_TARGET) {
    const config = JSON.parse(
      readFileSync(join(desktopDir, "tauri.conf.json"), "utf8"),
    );
    const minimum = config?.bundle?.macOS?.minimumSystemVersion;
    if (typeof minimum === "string" && minimum) {
      env.MACOSX_DEPLOYMENT_TARGET = minimum;
    }
  }
  return env;
}
const cargoEnv = cargoEnvironment();

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

// The sidecar binaries this hook stages, with the Cargo package that owns each.
//
// The host broker is the desktop's existing sidecar: it owns the per-workspace
// process tree for code executions. The CLI binary gives every Tidebreak
// desktop session a canonical `tidebreak` command on PATH so provider harnesses
// (Claude, Codex, OpenCode) can invoke `tidebreak browser-mcp` or other
// agent-side commands through a session-scoped capfile without asking the user
// to install or find anything.
const SIDECARS = [
  { binary: "tidebreak-host-broker", package: "tidebreak-host-broker" },
  { binary: "tidebreak", package: "tidebreak-cli" },
];

/**
 * Compile every sidecar for one target in a single Cargo invocation.
 *
 * The invocation selects the desktop package alongside the sidecar packages
 * but builds only the sidecar binaries. Cargo unifies features across the
 * selected packages, so the shared dependency graph resolves exactly as it
 * does when Tauri compiles the desktop crate right after this hook. Building
 * the sidecars on their own resolves a different graph (Tauri pulls extra
 * features into shared crates such as reqwest and tokio-util), and Cargo then
 * recompiles tidebreak-core and every crate above it a second time.
 *
 * Tauri passes `tauri/custom-protocol` to its own release build and nothing
 * extra to its dev build, so this hook mirrors that per profile.
 */
function buildSidecars(target) {
  const cargoArgs = ["build", "-p", "tidebreak-desktop"];
  for (const sidecar of SIDECARS) cargoArgs.push("-p", sidecar.package);
  for (const sidecar of SIDECARS) cargoArgs.push("--bin", sidecar.binary);
  cargoArgs.push("--locked");
  if (release) cargoArgs.push("--release", "--features", "tauri/custom-protocol");
  if (configuredTarget) cargoArgs.push("--target", target);
  execFileSync("cargo", cargoArgs, {
    cwd: workspaceDir,
    stdio: "inherit",
    env: cargoEnv,
  });
}

/**
 * Copy one compiled sidecar into the Tauri sidecar directory under its
 * target-suffixed name, then lipo the per-target copies for a universal build.
 */
function stageBinary(binaryName) {
  const stagedSidecars = [];
  for (const target of targets) {
    const extension = target.includes("windows") ? ".exe" : "";
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

for (const target of targets) buildSidecars(target);
for (const sidecar of SIDECARS) stageBinary(sidecar.binary);

// Native computer use depends on the separately signed Swift helper.
stageComputerUseHelper({ desktopDir, workspaceDir, targetRoot, triple, release });
